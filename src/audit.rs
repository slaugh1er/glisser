//! Auditing the dictionary without spending a call: where it is noisy
//! (`terms`) and where it buys windows and brings back nothing (`rules`).

use anyhow::Result;
use rusqlite::params;
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::db::Db;
use crate::triage::Verdict;

/// Frequency slice over terms, with samples.
pub fn terms(db: &Db, min: usize, samples: usize, filter: Option<&str>) -> Result<String> {
    let mut stmt = db.conn.prepare(
        "SELECT axis, rule_id, term, COUNT(*) n,
                COUNT(DISTINCT dialog_id) dlgs,
                MAX(cnt) top
         FROM (SELECT axis, rule_id, term, dialog_id,
                      COUNT(*) OVER (PARTITION BY rule_id, term, dialog_id) cnt
               FROM hits)
         GROUP BY axis, rule_id, term
         ORDER BY n DESC",
    )?;
    let rows: Vec<(String, String, String, i64, i64, i64)> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<Result<_, _>>()?;

    let mut out = String::new();
    out.push_str("term                  n    dialogs   worst dialog   share there\n");

    for (axis, rule_id, term, n, dlgs, top) in &rows {
        if (*n as usize) < min {
            continue;
        }
        if let Some(f) = filter
            && !term.contains(f)
            && !rule_id.contains(f)
            && !axis.contains(f)
        {
            continue;
        }
        // No automatic verdicts: concentration in one dialog describes a
        // word-boundary artefact and a real topic discussed with one person
        // equally well. The samples judge; the numbers only point.
        out.push_str(&format!(
            "{term:<20} {n:<5} {dlgs:<9} {top:<14} {:.0}%\n",
            *top as f64 / *n as f64 * 100.0
        ));

        if samples > 0 {
            for s in sample(db, rule_id, term, samples)? {
                out.push_str(&format!("    {s}\n"));
            }
        }
    }
    Ok(out)
}

/// Random samples: the matched surface plus the head of the message — the
/// surface alone cannot tell a real hit from one glued across a space.
fn sample(db: &Db, rule_id: &str, term: &str, k: usize) -> Result<Vec<String>> {
    let mut stmt = db.conn.prepare_cached(
        "SELECT h.surface, m.text FROM hits h
         JOIN messages m ON m.dialog_id = h.dialog_id AND m.msg_id = h.msg_id
         WHERE h.rule_id = ?1 AND h.term = ?2
         ORDER BY RANDOM() LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![rule_id, term, k as i64], |r| {
        Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (surface, text) = row?;
        let head: String = text.replace('\n', " ").chars().take(110).collect();
        out.push(format!("«{}» | {head}", surface.unwrap_or_default()));
    }
    Ok(out)
}

/// Feedback from triage: the share of a rule's hits the model actually cut.
/// A low share at a high volume means the rule pays for windows and brings
/// back no verdict.
pub fn rules(db: &Db, model: Option<&str>) -> Result<String> {
    let mut stmt = db.conn.prepare(
        "SELECT dialog_id, window_from, window_to, raw FROM triage_runs
         WHERE ?1 IS NULL OR model = ?1",
    )?;
    let rows: Vec<(i64, i32, i32, String)> = stmt
        .query_map(params![model], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<_, _>>()?;

    if rows.is_empty() {
        return Ok("no triage runs — the signal comes from calls already made\n".into());
    }

    let mut spans: HashMap<i64, Vec<(i32, i32)>> = HashMap::new();
    let mut cut: HashMap<i64, HashSet<i32>> = HashMap::new();
    for (dialog_id, from_id, to_id, raw) in &rows {
        spans
            .entry(*dialog_id)
            .or_default()
            .push((*from_id, *to_id));
        let Ok(v) = serde_json::from_str::<Verdict>(raw) else {
            continue;
        };
        let set = cut.entry(*dialog_id).or_default();
        for r in &v.delete_ranges {
            let (lo, hi) = if r.from <= r.to {
                (r.from, r.to)
            } else {
                (r.to, r.from)
            };
            let c = |x: i64| x.clamp(*from_id as i64, *to_id as i64) as i32;
            for id in c(lo)..=c(hi) {
                set.insert(id);
            }
        }
    }

    let mut stmt = db
        .conn
        .prepare("SELECT dialog_id, msg_id, rule_id, term FROM hits")?;
    let hits = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i32>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;

    let mut tally: BTreeMap<(String, String), (usize, usize)> = BTreeMap::new();
    for h in hits {
        let (dialog_id, msg_id, rule_id, term) = h?;
        let Some(list) = spans.get(&dialog_id) else {
            continue;
        };
        if !list.iter().any(|&(a, b)| msg_id >= a && msg_id <= b) {
            continue;
        }
        let e = tally.entry((rule_id, term)).or_default();
        e.0 += 1;
        if cut.get(&dialog_id).is_some_and(|s| s.contains(&msg_id)) {
            e.1 += 1;
        }
    }

    let mut list: Vec<_> = tally
        .into_iter()
        .filter(|(_, (n, _))| *n >= 3)
        .map(|((rule, term), (n, k))| (k as f64 / n as f64, n, k, rule, term))
        .collect();
    list.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then(b.1.cmp(&a.1)));

    let mut out = String::from(
        "share of hits that went under the knife (bottom: candidates to drop):\n\
         share  in windows  cut       rule / term\n",
    );
    for (share, n, k, rule, term) in &list {
        out.push_str(&format!(
            "{:>4.0}%  {n:<8} {k:<9} {rule} / {term}\n",
            share * 100.0
        ));
    }
    Ok(out)
}
