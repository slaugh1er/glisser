//! Quality of the triage: maximum recall while the deletion stays inside the
//! budget. Not F1 — a miss costs without limit, an excess cut is not free
//! either. Every number is a property of the verdict, none needs labelling.

use anyhow::Result;
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::config;
use crate::db::Db;
use crate::triage::Verdict;

type Ids = HashSet<(i64, i32)>;
/// What one run spent: key, dollars, prompt and completion tokens, widenings.
type Spend = (String, f64, i64, i64, i64);

/// One verdict, by one model, on one window.
struct Run {
    key: String,
    dialog_id: i64,
    from_id: i32,
    to_id: i32,
    deleted: Vec<(i32, i32)>,
    /// The real message ids of the window. Counting `from..to` would inflate
    /// «cut» into thousands: ids come with gaps.
    msgs: HashSet<i32>,
    protected_claims: usize,
}

/// Totals per run (model + policy version).
#[derive(Default)]
struct Agg {
    windows: usize,
    in_windows: usize,
    cut: usize,
    violations: usize,
    full: usize,
    empty: usize,
    claims: usize,
    anchors: usize,
    covered: usize,
    /// A range hit the edge of the window — the conversation is probably cut.
    edge: usize,
    /// The model asked for more context itself.
    expanded: usize,
    cost: f64,
    prompt_tokens: i64,
    completion_tokens: i64,
}

pub fn report(db: &Db) -> Result<String> {
    let runs = load_runs(db)?;
    if runs.is_empty() {
        return Ok("triage has not run yet — nothing to score\n".into());
    }

    let protected = load_protected(db)?;
    // Windows are rebuilt only for the anchors; the rest is read off the
    // verdicts themselves.
    let mut anchors: HashMap<(i64, i32, i32), Vec<i32>> = HashMap::new();
    let mut sizes: HashMap<(i64, i32, i32), usize> = HashMap::new();
    for w in crate::window::build(db)? {
        let k = (w.dialog_id, w.from_id, w.to_id);
        anchors.insert(k, w.anchors.clone());
        sizes.insert(k, w.total);
    }

    let mut by_key: BTreeMap<String, Agg> = BTreeMap::new();
    for r in &runs {
        let a = by_key.entry(r.key.clone()).or_default();
        let k = (r.dialog_id, r.from_id, r.to_id);
        a.windows += 1;
        a.in_windows += sizes.get(&k).copied().unwrap_or(0);
        a.claims += r.protected_claims;
        if r.deleted.is_empty() {
            a.empty += 1;
        }

        let ids = expand(r);
        a.cut += ids.len();
        a.violations += ids
            .iter()
            .filter(|&&id| protected.contains(&(r.dialog_id, id)))
            .count();
        if let Some(n) = sizes.get(&k)
            && ids.len() as f64 / (*n).max(1) as f64 > 0.9
        {
            a.full += 1;
        }
        // Edge of the window: a direct measure of whether it is big enough.
        if let (Some(&lo), Some(&hi)) = (r.msgs.iter().min(), r.msgs.iter().max())
            && (ids.contains(&lo) || ids.contains(&hi))
        {
            a.edge += 1;
        }
        if let Some(list) = anchors.get(&k) {
            a.anchors += list.len();
            a.covered += list.iter().filter(|id| ids.contains(id)).count();
        }
    }

    for (key, cost, pt, ct, exp) in load_spend(db)? {
        if let Some(a) = by_key.get_mut(&key) {
            a.cost = cost;
            a.prompt_tokens = pt;
            a.completion_tokens = ct;
            a.expanded = exp as usize;
        }
    }

    let mut out = String::new();
    out.push_str("runs (model @ policy version):\n\n");
    for (key, a) in &by_key {
        let pct = |x: usize, of: usize| {
            if of > 0 {
                x as f64 / of as f64 * 100.0
            } else {
                0.0
            }
        };
        out.push_str(&format!("{key}\n"));
        out.push_str(&format!(
            "  windows {:<5} cut {:>6} of {:>6} in windows ({:.1}%)\n",
            a.windows,
            a.cut,
            a.in_windows,
            pct(a.cut, a.in_windows)
        ));
        out.push_str(&format!(
            "  whitelist violations {:<4} <- must be 0, or the policy fails to carry it\n",
            a.violations
        ));
        out.push_str(&format!(
            "  anchor coverage {:.0}%   empty verdicts {:.0}% <- high: the dictionary is noisy\n",
            pct(a.covered, a.anchors),
            pct(a.empty, a.windows)
        ));
        out.push_str(&format!(
            "  windows cut whole {:<4} <- many: the model is not discriminating\n",
            a.full
        ));
        out.push_str(&format!(
            "  range hit the edge {:.0}%   asked to widen {:.0}% <- the window is small\n",
            pct(a.edge, a.windows),
            pct(a.expanded, a.windows)
        ));
        out.push_str(&format!(
            "  deliberate keeps {:<4} <- zero is suspicious: the presumption did not land\n",
            a.claims
        ));
        out.push_str(&format!(
            "  ${:.3}  tokens {}/{}\n\n",
            a.cost, a.prompt_tokens, a.completion_tokens
        ));
    }

    out.push_str(&agreement(&runs));
    out.push_str(&plan_budget(db)?);
    Ok(out)
}

/// The budget is measured on `plan`, i.e. on the one run that was chosen.
fn plan_budget(db: &Db) -> Result<String> {
    let planned: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM plan", [], |r| r.get(0))?;
    if planned == 0 {
        return Ok("\nthe plan is empty — build it with `plan --model ...`\n".into());
    }
    let corpus: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
    let ratio = planned as f64 / corpus.max(1) as f64;

    let mut out = format!(
        "\n--- plan: {planned} of {corpus} ({:.2}%) ---\n",
        ratio * 100.0
    );

    let mut stmt = db.conn.prepare(
        "SELECT d.title, COUNT(p.msg_id) k, d.msg_count
         FROM plan p JOIN dialogs d ON d.id = p.dialog_id
         GROUP BY p.dialog_id
         ORDER BY (CAST(COUNT(p.msg_id) AS REAL) / MAX(d.msg_count,1)) DESC
         LIMIT 8",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut worst = 0.0f64;
    out.push_str("worst dialogs by share deleted:\n");
    for row in rows {
        let (title, k, total) = row?;
        let p = k as f64 / total.max(1) as f64;
        worst = worst.max(p);
        out.push_str(&format!(
            "  {:>6.1}%  {k:>6} / {total:<7} {title}\n",
            p * 100.0
        ));
    }

    check(&mut out, "share of corpus", ratio, config::MAX_DELETE_RATIO);
    check(&mut out, "worst dialog", worst, config::MAX_DIALOG_RATIO);
    Ok(out)
}

fn check(out: &mut String, name: &str, got: f64, limit: f64) {
    let mark = if got <= limit { "ok      " } else { "EXCEEDED" };
    out.push_str(&format!(
        "  {mark}  {name}: {:.2}% against a limit of {:.2}%\n",
        got * 100.0,
        limit * 100.0
    ));
}

/// Jaccard between runs on identical windows. Different model families
/// disagree meaningfully — and only those windows are worth a human.
fn agreement(runs: &[Run]) -> String {
    let mut by_window: HashMap<(i64, i32, i32), Vec<&Run>> = HashMap::new();
    for r in runs {
        by_window
            .entry((r.dialog_id, r.from_id, r.to_id))
            .or_default()
            .push(r);
    }

    let mut pairs: BTreeMap<(String, String), (f64, usize, usize)> = BTreeMap::new();
    let mut split = Vec::new();
    for (w, group) in &by_window {
        for i in 0..group.len() {
            for j in i + 1..group.len() {
                let (a, b) = (group[i], group[j]);
                let (m1, m2) = if a.key <= b.key {
                    (a.key.clone(), b.key.clone())
                } else {
                    (b.key.clone(), a.key.clone())
                };
                let (sa, sb) = (expand(a), expand(b));
                let union = sa.union(&sb).count();
                let js = if union == 0 {
                    1.0
                } else {
                    sa.intersection(&sb).count() as f64 / union as f64
                };
                let e = pairs.entry((m1, m2)).or_insert((0.0, 0, 0));
                e.0 += js;
                e.1 += 1;
                if js < 0.5 {
                    e.2 += 1;
                    split.push(*w);
                }
            }
        }
    }

    if pairs.is_empty() {
        return "\nagreement: only one run — triage the same set with another model\n".into();
    }

    let mut out = String::from("\nagreement (Jaccard over message ids):\n");
    for ((m1, m2), (sum, n, low)) in pairs {
        out.push_str(&format!(
            "  {:.2}  {m1} <-> {m2}  ({n} windows, far apart on {low})\n",
            sum / n as f64
        ));
    }

    split.sort_unstable();
    split.dedup();
    if !split.is_empty() {
        out.push_str(&format!(
            "\nwindows for a human ({}) — only these need one:\n",
            split.len()
        ));
        for (d, f, t) in split.iter().take(15) {
            out.push_str(&format!("  {d}:{f}..{t}\n"));
        }
    }
    out
}

fn expand(r: &Run) -> HashSet<i32> {
    r.msgs
        .iter()
        .copied()
        .filter(|id| r.deleted.iter().any(|&(lo, hi)| *id >= lo && *id <= hi))
        .collect()
}

fn load_runs(db: &Db) -> Result<Vec<Run>> {
    let mut stmt = db.conn.prepare(
        "SELECT dialog_id, window_from, window_to, model, prompt_id, raw FROM triage_runs",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i32>(1)?,
            r.get::<_, i32>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
        ))
    })?;

    let rows: Vec<_> = rows.collect::<Result<Vec<_>, _>>()?;
    let mut ids = db
        .conn
        .prepare("SELECT msg_id FROM messages WHERE dialog_id=?1 AND msg_id BETWEEN ?2 AND ?3")?;

    let mut out = Vec::new();
    for row in rows {
        let (dialog_id, from_id, to_id, model, prompt_id, raw) = row;
        let v: Verdict = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("unparsed verdict {dialog_id}:{from_id}: {e}");
                continue;
            }
        };
        let msgs: HashSet<i32> = ids
            .query_map(rusqlite::params![dialog_id, from_id, to_id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        out.push(Run {
            key: format!("{model} @ {prompt_id}"),
            dialog_id,
            from_id,
            to_id,
            msgs,
            deleted: v
                .delete_ranges
                .iter()
                .map(|r| {
                    let c = |x: i64| x.clamp(from_id as i64, to_id as i64) as i32;
                    if r.from <= r.to {
                        (c(r.from), c(r.to))
                    } else {
                        (c(r.to), c(r.from))
                    }
                })
                .collect(),
            protected_claims: v.protected.len(),
        });
    }
    Ok(out)
}

fn load_spend(db: &Db) -> Result<Vec<Spend>> {
    let mut stmt = db.conn.prepare(
        "SELECT model || ' @ ' || prompt_id, SUM(cost),
                SUM(prompt_tokens), SUM(completion_tokens),
                SUM(CASE WHEN passes > 1 THEN 1 ELSE 0 END)
         FROM triage_runs GROUP BY model, prompt_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Messages held by the whitelist.
fn load_protected(db: &Db) -> Result<Ids> {
    let mut stmt = db
        .conn
        .prepare("SELECT dialog_id, msg_id FROM messages WHERE protected = 1")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}
