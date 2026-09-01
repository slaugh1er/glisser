//! The difference between two models' runs, from the stored verdicts. Where
//! both agreed there is nothing to review; worth a human are the tails.

use anyhow::{Result, bail};
use rusqlite::params;
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::db::Db;
use crate::triage::Verdict;

/// Messages doomed by one run: id → the reason the model gave.
type Marks = HashMap<(i64, i32), String>;
/// Window keys, as `triage_runs` stores them: dialog, from, to.
type Windows = HashSet<(i64, i32, i32)>;

pub fn report(db: &Db, base: &str, against: &str, show: usize) -> Result<String> {
    let (a, wa) = load(db, base)?;
    let (b, wb) = load(db, against)?;
    if a.is_empty() || b.is_empty() {
        bail!("one of the models has no runs — triage it first");
    }

    // A fair comparison is only possible on the windows both models saw.
    let common: HashSet<_> = wa.intersection(&wb).copied().collect();
    let keep = |m: &Marks| -> Marks {
        m.iter()
            .filter(|((d, id), _)| {
                common
                    .iter()
                    .any(|&(cd, f, t)| cd == *d && *id >= f && *id <= t)
            })
            .map(|(k, v)| (*k, v.clone()))
            .collect()
    };
    let (a, b) = (keep(&a), keep(&b));

    let ka: HashSet<_> = a.keys().copied().collect();
    let kb: HashSet<_> = b.keys().copied().collect();
    let both = ka.intersection(&kb).count();
    let only_a: Vec<_> = ka.difference(&kb).copied().collect();
    let only_b: Vec<_> = kb.difference(&ka).copied().collect();

    let mut out = format!(
        "windows in common: {}\n\n\
         agreed (both assigned)  : {both}\n\
         only {base:<24} : {}\n\
         only {against:<24} : {}\n",
        common.len(),
        only_a.len(),
        only_b.len()
    );
    let union = both + only_a.len() + only_b.len();
    if union > 0 {
        out.push_str(&format!(
            "\nundisputed share: {:.0}% — deletable without a second look\n",
            both as f64 / union as f64 * 100.0
        ));
    }

    if show > 0 {
        out.push_str(&format!(
            "\n=== assigned only by {against} ({} messages) ===\n\
             either its false positives or a seam {base} misses\n",
            only_b.len()
        ));
        out.push_str(&dump(db, &b, &only_b, show)?);
    }
    Ok(out)
}

/// Messages of one run with their reason. Windows come from the
/// `triage_runs` key, so ranges clamp to exactly what the model saw.
fn load(db: &Db, model: &str) -> Result<(Marks, Windows)> {
    let mut stmt = db.conn.prepare(
        "SELECT dialog_id, window_from, window_to, raw FROM triage_runs WHERE model = ?1",
    )?;
    let rows: Vec<(i64, i32, i32, String)> = stmt
        .query_map(params![model], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<_, _>>()?;

    let mut marks = Marks::new();
    let mut windows = HashSet::new();
    let mut ids = db
        .conn
        .prepare("SELECT msg_id FROM messages WHERE dialog_id=?1 AND msg_id BETWEEN ?2 AND ?3")?;

    for (d, f, t, raw) in rows {
        windows.insert((d, f, t));
        let Ok(v) = serde_json::from_str::<Verdict>(&raw) else {
            continue;
        };
        for r in &v.delete_ranges {
            let c = |x: i64| x.clamp(f as i64, t as i64) as i32;
            let (lo, hi) = if r.from <= r.to {
                (c(r.from), c(r.to))
            } else {
                (c(r.to), c(r.from))
            };
            for id in ids.query_map(params![d, lo, hi], |x| x.get::<_, i32>(0))? {
                marks.insert((d, id?), r.reason.clone());
            }
        }
    }
    Ok((marks, windows))
}

fn dump(db: &Db, marks: &Marks, keys: &[(i64, i32)], show: usize) -> Result<String> {
    // Grouped by reason: a tail of a hundred messages is usually five calls.
    let mut by_reason: BTreeMap<&str, Vec<(i64, i32)>> = BTreeMap::new();
    for k in keys {
        by_reason.entry(marks[k].as_str()).or_default().push(*k);
    }
    let mut groups: Vec<_> = by_reason.into_iter().collect();
    groups.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));

    let mut out = String::new();
    for (reason, mut ids) in groups.into_iter().take(show) {
        ids.sort_unstable();
        out.push_str(&format!("\n[{} msgs] {reason}\n", ids.len()));
        for (d, id) in ids.iter().take(3) {
            let row: rusqlite::Result<(String, i64, String)> = db.conn.query_row(
                "SELECT COALESCE(dl.title,'?'), m.date, m.text
                 FROM messages m JOIN dialogs dl ON dl.id = m.dialog_id
                 WHERE m.dialog_id=?1 AND m.msg_id=?2",
                params![d, id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            );
            if let Ok((title, date, text)) = row {
                let day = chrono::DateTime::from_timestamp(date, 0)
                    .map(|x| x.format("%Y-%m-%d").to_string())
                    .unwrap_or_default();
                let head: String = text.replace('\n', " ").chars().take(100).collect();
                out.push_str(&format!("    {day} «{title}» {head}\n"));
            }
        }
    }
    Ok(out)
}
