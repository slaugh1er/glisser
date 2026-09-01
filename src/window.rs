//! Clustering hits into windows: one window is one LLM call.
//!
//! A window is measured in messages, not ids: ids come with gaps, so «±30»
//! by id would be an arbitrary number of messages.

use anyhow::Result;
use rusqlite::params;
use std::collections::{BTreeMap, BTreeSet};

use crate::config;
use crate::db::Db;
use crate::model::Tier;

#[derive(Debug, Clone)]
pub struct Window {
    pub dialog_id: i64,
    pub from_id: i32,
    pub to_id: i32,
    /// The messages that made this window exist.
    pub anchors: Vec<i32>,
    pub axes: BTreeSet<String>,
    /// Harshest tier among the anchors — it goes into the prompt as a label.
    pub tier: Tier,
    /// Voice messages inside the window: only these go to transcription.
    pub voices: Vec<i32>,
    pub total: usize,
    /// Characters of text inside the window — an estimate of the prompt size.
    pub chars: usize,
    /// Date of the newest anchor and the rules the anchors fired on. Used by
    /// `Cutoff` only.
    pub newest: i64,
    pub rules: BTreeSet<String>,
}

/// Selection for a repeat run: keep a window if its anchors reach material
/// newer than the cutoff, or if a rule fired that was not in the dictionary
/// last time. Counted over anchors, because an anchor is the reason for the
/// call and the ±30 messages of context are not.
pub struct Cutoff {
    pub since: i64,
    pub new_rules: BTreeSet<String>,
}

/// A message with its hits aggregated.
struct Row {
    msg_id: i32,
    date: i64,
    priority: f64,
    protected: bool,
    axes: BTreeSet<String>,
    rules: BTreeSet<String>,
    tier: Option<Tier>,
    voice: bool,
    chars: usize,
}

pub fn build(db: &Db) -> Result<Vec<Window>> {
    let ids: Vec<(i64, String)> = db
        .conn
        .prepare("SELECT id, kind FROM dialogs ORDER BY id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;

    let mut out = Vec::new();
    for (did, kind) in ids {
        let ignored =
            config::IGNORE_DIALOGS.contains(&did) || config::IGNORE_KINDS.contains(&kind.as_str());
        if !ignored {
            out.extend(build_dialog(db, did)?);
        }
    }
    Ok(out)
}

/// Widen a window on the model's request, up to `MAX_WINDOW`. By position.
pub fn expand(db: &Db, w: &Window, before: i64, after: i64) -> Result<Window> {
    let room = (config::MAX_WINDOW - w.total as i64).max(0);
    if room == 0 {
        return Ok(w.clone());
    }
    // The request is split proportionally, so one side cannot eat the ceiling.
    let asked = (before.max(0) + after.max(0)).max(1);
    let scale = |x: i64| (x.max(0) * room / asked).max(0);

    let mut out = w.clone();
    out.from_id = step(db, w.dialog_id, w.from_id, scale(before), true)?;
    out.to_id = step(db, w.dialog_id, w.to_id, scale(after), false)?;
    let (total, chars) = measure(db, w.dialog_id, out.from_id, out.to_id)?;
    out.total = total;
    out.chars = chars;
    out.voices = voices(db, w.dialog_id, out.from_id, out.to_id)?;
    Ok(out)
}

/// The id `n` messages before/after this one. The edge is not an error.
fn step(db: &Db, dialog_id: i64, from: i32, n: i64, back: bool) -> Result<i32> {
    if n == 0 {
        return Ok(from);
    }
    let sql = if back {
        "SELECT msg_id FROM messages WHERE dialog_id=?1 AND msg_id < ?2
         ORDER BY msg_id DESC LIMIT 1 OFFSET ?3"
    } else {
        "SELECT msg_id FROM messages WHERE dialog_id=?1 AND msg_id > ?2
         ORDER BY msg_id ASC LIMIT 1 OFFSET ?3"
    };
    let got: Option<i32> = db
        .conn
        .prepare_cached(sql)?
        .query_row(params![dialog_id, from, n - 1], |r| r.get(0))
        .ok();
    Ok(match got {
        Some(id) => id,
        None => edge(db, dialog_id, back)?.unwrap_or(from),
    })
}

fn edge(db: &Db, dialog_id: i64, first: bool) -> Result<Option<i32>> {
    let sql = if first {
        "SELECT MIN(msg_id) FROM messages WHERE dialog_id=?1"
    } else {
        "SELECT MAX(msg_id) FROM messages WHERE dialog_id=?1"
    };
    Ok(db
        .conn
        .prepare_cached(sql)?
        .query_row(params![dialog_id], |r| r.get(0))?)
}

fn measure(db: &Db, dialog_id: i64, from: i32, to: i32) -> Result<(usize, usize)> {
    let (n, c): (i64, i64) = db
        .conn
        .prepare_cached(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(text)),0) FROM messages
         WHERE dialog_id=?1 AND msg_id BETWEEN ?2 AND ?3",
        )?
        .query_row(params![dialog_id, from, to], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok((n as usize, c as usize))
}

fn voices(db: &Db, dialog_id: i64, from: i32, to: i32) -> Result<Vec<i32>> {
    let mut stmt = db.conn.prepare_cached(
        "SELECT msg_id FROM messages
         WHERE dialog_id=?1 AND msg_id BETWEEN ?2 AND ?3
           AND media_type IN ('voice_message','video_message')",
    )?;
    let rows = stmt.query_map(params![dialog_id, from, to], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn build_dialog(db: &Db, dialog_id: i64) -> Result<Vec<Window>> {
    let rows = load(db, dialog_id)?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let anchors: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| is_anchor(r))
        .map(|(i, _)| i)
        .collect();
    if anchors.is_empty() {
        return Ok(Vec::new());
    }

    let gap = config::CLUSTER_GAP.max(0) as usize;
    let pad = config::WINDOW.max(0) as usize;

    let mut clusters: Vec<(usize, usize, Vec<usize>)> = Vec::new();
    for &a in &anchors {
        match clusters.last_mut() {
            Some((_, end, members)) if a - *end <= gap => {
                *end = a;
                members.push(a);
            }
            _ => clusters.push((a, a, vec![a])),
        }
    }

    let mut windows = Vec::new();
    for (start, end, members) in clusters {
        let lo = start.saturating_sub(pad);
        let hi = (end + pad).min(rows.len() - 1);

        let mut axes = BTreeSet::new();
        let mut rules = BTreeSet::new();
        let mut newest = 0i64;
        let mut tier = Tier::T2;
        for &m in &members {
            axes.extend(rows[m].axes.iter().cloned());
            rules.extend(rows[m].rules.iter().cloned());
            newest = newest.max(rows[m].date);
            if let Some(t) = rows[m].tier {
                tier = tier.min(t); // T0 < T1 < T2, keep the harshest
            }
        }

        windows.push(Window {
            dialog_id,
            from_id: rows[lo].msg_id,
            to_id: rows[hi].msg_id,
            anchors: members.iter().map(|&m| rows[m].msg_id).collect(),
            axes,
            tier,
            voices: (lo..=hi)
                .filter(|&i| rows[i].voice)
                .map(|i| rows[i].msg_id)
                .collect(),
            total: hi - lo + 1,
            chars: (lo..=hi).map(|i| rows[i].chars).sum(),
            newest,
            rules,
        });
    }

    Ok(windows)
}

/// Summed priority plus the whitelist. Priority only decides whether a model
/// call is worth spending — the verdict is the model's.
fn is_anchor(r: &Row) -> bool {
    !r.protected && r.priority >= config::HIT_THRESHOLD
}

fn load(db: &Db, dialog_id: i64) -> Result<Vec<Row>> {
    let mut stmt = db.conn.prepare_cached(
        // Protection is read from messages.protected, the single source of
        // truth: the whitelist marks a message, not a detector hit.
        "SELECT m.msg_id,
                m.date,
                COALESCE(SUM(h.priority), 0)  AS priority,
                m.protected,
                GROUP_CONCAT(DISTINCT h.axis) AS axes,
                GROUP_CONCAT(DISTINCT h.rule_id) AS rules,
                MIN(h.tier)                   AS tier,
                m.media_type,
                LENGTH(m.text)                AS chars
         FROM messages m
         LEFT JOIN hits h ON h.dialog_id = m.dialog_id AND h.msg_id = m.msg_id
         WHERE m.dialog_id = ?1
         GROUP BY m.msg_id
         ORDER BY m.msg_id",
    )?;

    let rows = stmt.query_map(params![dialog_id], |r| {
        let split = |s: Option<String>| -> BTreeSet<String> {
            s.map(|s| s.split(',').map(str::to_string).collect())
                .unwrap_or_default()
        };
        let media: Option<String> = r.get(7)?;
        Ok(Row {
            msg_id: r.get(0)?,
            date: r.get(1)?,
            priority: r.get(2)?,
            protected: r.get::<_, i64>(3)? == 1,
            axes: split(r.get(4)?),
            rules: split(r.get(5)?),
            tier: r
                .get::<_, Option<String>>(6)?
                .as_deref()
                .and_then(Tier::parse),
            voice: matches!(
                media.as_deref(),
                Some("voice_message") | Some("video_message")
            ),
            chars: r.get::<_, i64>(8)? as usize,
        })
    })?;

    Ok(rows.collect::<Result<_, _>>()?)
}

/// Which windows a run takes. Deterministic: two models with the same flags
/// must see the same set, or a bake-off compares samples, not models.
///
/// `limit` takes the head — the freshest and sharpest; `sample` stratifies by
/// axis, because comparing models needs spread. `cutoff` is applied first, or
/// the head of the list would be spent on material already paid for.
pub fn select(
    mut windows: Vec<Window>,
    limit: Option<usize>,
    sample: Option<usize>,
    cutoff: Option<&Cutoff>,
) -> Vec<Window> {
    if let Some(c) = cutoff {
        windows.retain(|w| w.newest >= c.since || w.rules.iter().any(|r| c.new_rules.contains(r)));
    }

    windows.sort_by(|a, b| {
        (
            a.tier,
            std::cmp::Reverse(a.anchors.len()),
            a.dialog_id,
            a.from_id,
        )
            .cmp(&(
                b.tier,
                std::cmp::Reverse(b.anchors.len()),
                b.dialog_id,
                b.from_id,
            ))
    });

    if let Some(n) = sample {
        let mut by_axis: BTreeMap<String, Vec<Window>> = BTreeMap::new();
        for w in windows {
            let axis = w.axes.iter().next().cloned().unwrap_or_default();
            by_axis.entry(axis).or_default().push(w);
        }
        let mut axes: Vec<Vec<Window>> = by_axis.into_values().collect();
        axes.iter_mut().for_each(|g| g.sort_by_key(spread));

        // Round-robin: the i-th window of every axis, in turn, until we have n.
        let deepest = axes.iter().map(Vec::len).max().unwrap_or(0);
        let groups = &axes;
        let mut out: Vec<Window> = (0..deepest)
            .flat_map(move |i| groups.iter().filter_map(move |g| g.get(i).cloned()))
            .take(n)
            .collect();
        out.sort_by_key(|w| (w.dialog_id, w.from_id));
        return out;
    }

    if let Some(n) = limit {
        windows.truncate(n);
    }
    windows
}

/// Stable spread inside an axis: without it the sample slides into the one or
/// two most talkative dialogs.
fn spread(w: &Window) -> u32 {
    w.dialog_id
        .to_le_bytes()
        .iter()
        .chain(w.from_id.to_le_bytes().iter())
        .fold(2_166_136_261u32, |h, b| {
            (h ^ *b as u32).wrapping_mul(16_777_619)
        })
}

/// How many model calls lie ahead, and on what.
pub fn report(windows: &[Window]) -> String {
    let mut out = String::new();
    let msgs: usize = windows.iter().map(|w| w.total).sum();
    let voices: usize = windows.iter().map(|w| w.voices.len()).sum();
    let chars: usize = windows.iter().map(|w| w.chars).sum();

    out.push_str(&format!("windows (LLM calls): {}\n", windows.len()));
    out.push_str(&format!("messages in windows: {msgs}\n"));

    // Window size drives both price and quality: on a huge context the model
    // starts cutting in slabs instead of aimed ranges.
    let mut sizes: Vec<usize> = windows.iter().map(|w| w.total).collect();
    sizes.sort_unstable();
    let pct = |p: f64| sizes[((sizes.len() as f64 - 1.0) * p) as usize];
    if !sizes.is_empty() {
        out.push_str(&format!(
            "window size        : med {}  p90 {}  p99 {}  max {}\n",
            pct(0.5),
            pct(0.9),
            pct(0.99),
            sizes[sizes.len() - 1]
        ));
    }
    out.push_str(&format!("voice to transcribe: {voices}\n"));
    // ~2.5 Cyrillic characters per token, plus the line overhead and the policy.
    let toks = chars / 2 + windows.len() * (60 * 25 / 2 + 1200);
    out.push_str(&format!(
        "text in windows    : {:.1}M characters\n",
        chars as f64 / 1e6
    ));
    out.push_str(&format!(
        "input tokens (est) : {:.2}M (~{} per call)\n",
        toks as f64 / 1e6,
        toks / windows.len().max(1)
    ));

    let mut by_tier: BTreeMap<&str, usize> = BTreeMap::new();
    for w in windows {
        *by_tier.entry(w.tier.as_str()).or_default() += 1;
    }
    out.push_str("\nby tier:\n");
    for (t, n) in by_tier {
        out.push_str(&format!("  {t}  {n}\n"));
    }

    let mut by_axis: BTreeMap<String, usize> = BTreeMap::new();
    for w in windows {
        for a in &w.axes {
            *by_axis.entry(a.clone()).or_default() += 1;
        }
    }
    out.push_str("\nby axis:\n");
    for (a, n) in by_axis {
        out.push_str(&format!("  {a:<14} {n}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(msg_id: i32, priority: f64, protected: bool) -> Row {
        Row {
            msg_id,
            date: 0,
            priority,
            protected,
            axes: BTreeSet::new(),
            rules: BTreeSet::new(),
            tier: Some(Tier::T0),
            voice: false,
            chars: 0,
        }
    }

    /// Clustering without a database: the logic is the interesting part.
    fn cluster(rows: &[Row], pad: usize, gap: usize) -> Vec<(usize, usize)> {
        let mut clusters: Vec<(usize, usize)> = Vec::new();
        for (a, _) in rows.iter().enumerate().filter(|(_, r)| is_anchor(r)) {
            match clusters.last_mut() {
                Some((_, end)) if a - *end <= gap => *end = a,
                _ => clusters.push((a, a)),
            }
        }
        clusters
            .into_iter()
            .map(|(s, e)| (s.saturating_sub(pad), (e + pad).min(rows.len() - 1)))
            .collect()
    }

    #[test]
    fn nearby_anchors_merge_into_one_window() {
        let mut rows: Vec<Row> = (0..100).map(|i| row(i, 0.0, false)).collect();
        rows[20].priority = 2.0;
        rows[25].priority = 2.0;
        rows[28].priority = 2.0;
        // Three anchors within the gap → one window, not three calls.
        assert_eq!(cluster(&rows, 5, 10), vec![(15, 33)]);
    }

    #[test]
    fn distant_anchors_stay_separate() {
        let mut rows: Vec<Row> = (0..100).map(|i| row(i, 0.0, false)).collect();
        rows[20].priority = 2.0;
        rows[60].priority = 2.0;
        assert_eq!(cluster(&rows, 5, 10), vec![(15, 25), (55, 65)]);
    }

    #[test]
    fn window_clamps_at_dialog_edges() {
        let mut rows: Vec<Row> = (0..10).map(|i| row(i, 0.0, false)).collect();
        rows[1].priority = 2.0;
        assert_eq!(cluster(&rows, 30, 10), vec![(0, 9)]);
    }

    #[test]
    fn protected_messages_never_anchor() {
        let mut rows: Vec<Row> = (0..50).map(|i| row(i, 0.0, false)).collect();
        rows[20].priority = 9.0;
        rows[20].protected = true;
        assert!(cluster(&rows, 5, 10).is_empty());
    }

    #[test]
    fn below_threshold_does_not_anchor() {
        let mut rows: Vec<Row> = (0..50).map(|i| row(i, 0.0, false)).collect();
        rows[20].priority = 0.6; // a damped T2 hit
        assert!(cluster(&rows, 5, 10).is_empty());
    }

    fn win(newest: i64, rules: &[&str]) -> Window {
        Window {
            dialog_id: 1,
            from_id: 1,
            to_id: 2,
            anchors: vec![1],
            axes: BTreeSet::new(),
            tier: Tier::T0,
            voices: Vec::new(),
            total: 2,
            chars: 0,
            newest,
            rules: rules.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// The old is already paid for, but a new rule lifts a window of any age.
    /// The sample walks the axes in turn, so a bake-off gets spread rather
    /// than thirty crypto windows in a row.
    #[test]
    fn sample_takes_one_window_per_axis_in_turn() {
        let of = |axis: &str, from_id: i32| Window {
            from_id,
            axes: [axis.to_string()].into_iter().collect(),
            ..win(0, &[])
        };
        let got = select(
            vec![
                of("crypto", 1),
                of("crypto", 2),
                of("crypto", 3),
                of("politics", 4),
            ],
            None,
            Some(3),
            None,
        );
        // two crypto windows and the single politics one, not three crypto
        assert_eq!(got.len(), 3);
        assert!(got.iter().any(|w| w.axes.contains("politics")));
        assert_eq!(got.iter().filter(|w| w.axes.contains("crypto")).count(), 2);
    }

    #[test]
    fn cutoff_keeps_fresh_and_new_rules_only() {
        let c = Cutoff {
            since: 100,
            new_rules: ["leaving_question".to_string()].into_iter().collect(),
        };
        let got = select(
            vec![
                win(50, &["intent"]),           // old, old rule — skipped
                win(150, &["intent"]),          // fresh — taken
                win(50, &["leaving_question"]), // old, new rule — taken
            ],
            None,
            None,
            Some(&c),
        );
        assert_eq!(got.len(), 2);
        assert!(
            got.iter()
                .all(|w| w.newest == 150 || w.rules.contains("leaving_question"))
        );
    }
}
