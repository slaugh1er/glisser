//! Corpus scan: dictionary, regexes, structural escalation. One dialog at a
//! time — the corpus does not fit in memory, and escalation never crosses a
//! dialog boundary anyway.

use anyhow::Result;
use rusqlite::params;
use std::collections::{HashMap, HashSet};

use crate::db::Db;
use crate::dict::Dictionary;
use crate::model::{self, Axis, Hit, Layer, Message, Tier};

#[derive(Debug, Default)]
pub struct Stats {
    pub dialogs: usize,
    pub messages: usize,
    pub hits: usize,
    pub structural: usize,
    pub protected: usize,
}

pub fn run(db: &mut Db, dict: &Dictionary, only_axis: Option<Axis>) -> Result<Stats> {
    let now = model::now();
    let ids: Vec<i64> = db
        .conn
        .prepare("SELECT id FROM dialogs ORDER BY id")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;

    let mut stats = Stats::default();

    for did in ids {
        let msgs = load_dialog(db, did)?;
        if msgs.is_empty() {
            continue;
        }
        stats.dialogs += 1;
        stats.messages += msgs.len();

        let (mut hits, protected) = keyword_pass(&msgs, dict, now, only_axis, &mut stats);
        stats.structural += structural_pass(&msgs, &mut hits, now);

        db.mark_protected(did, &protected)?;
        stats.hits += db.insert_hits(&hits)?;
    }

    Ok(stats)
}

fn load_dialog(db: &Db, dialog_id: i64) -> Result<Vec<Message>> {
    let mut stmt = db.conn.prepare_cached(
        "SELECT msg_id, date, from_id, outgoing, reply_to, grouped_id,
                fwd_from, media_type, file_path, text
         FROM messages WHERE dialog_id = ?1 ORDER BY msg_id",
    )?;
    let rows = stmt.query_map(params![dialog_id], |r| {
        Ok(Message {
            dialog_id,
            msg_id: r.get(0)?,
            date: r.get(1)?,
            from_id: r.get(2)?,
            outgoing: r.get::<_, i32>(3)? != 0,
            reply_to: r.get(4)?,
            grouped_id: r.get(5)?,
            fwd_from: r.get(6)?,
            media_type: r.get(7)?,
            file_path: r.get(8)?,
            text: r.get(9)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn keyword_pass(
    msgs: &[Message],
    dict: &Dictionary,
    now: i64,
    only_axis: Option<Axis>,
    stats: &mut Stats,
) -> (Vec<Hit>, Vec<i32>) {
    let mut hits = Vec::new();
    let mut protected_ids = Vec::new();

    for m in msgs {
        if m.text.is_empty() {
            continue;
        }
        let norm = crate::norm::Normalized::build(&m.text);

        // Checked before and independently of matching: a message the
        // dictionary never fired on still deserves protection.
        let protected = dict.is_protected(&norm);
        if protected {
            protected_ids.push(m.msg_id);
            stats.protected += 1;
        }

        let matches = dict.scan(&m.text, &norm);
        if matches.is_empty() {
            continue;
        }

        let tier = Tier::of(m.date, now);
        for mt in matches {
            if only_axis.is_some_and(|a| a != mt.axis) {
                continue;
            }
            hits.push(Hit {
                dialog_id: m.dialog_id,
                msg_id: m.msg_id,
                axis: mt.axis,
                rule_id: mt.rule_id,
                term: mt.term,
                surface: mt.surface,
                layer: mt.layer,
                tier,
                priority: mt.weight * tier.multiplier(),
                protected: protected && mt.axis != Axis::CryptoHard,
            });
        }
    }

    (hits, protected_ids)
}

/// Dialog topology: replies in both directions, and albums.
///
/// The backward direction is the point of this pass: voice messages are not
/// scanned, but if a risky message replies to one, that voice message belongs
/// in the window too.
fn structural_pass(msgs: &[Message], hits: &mut Vec<Hit>, now: i64) -> usize {
    let hit_ids: HashSet<i32> = hits.iter().map(|h| h.msg_id).collect();
    if hit_ids.is_empty() {
        return 0;
    }

    let by_id: HashMap<i32, &Message> = msgs.iter().map(|m| (m.msg_id, m)).collect();
    let mut added: HashMap<i32, &'static str> = HashMap::new();

    for m in msgs {
        if !hit_ids.contains(&m.msg_id) && m.reply_to.is_some_and(|r| hit_ids.contains(&r)) {
            added.insert(m.msg_id, "reply_to_hit");
        }
    }

    for h in hits.iter() {
        if let Some(parent) = by_id.get(&h.msg_id).and_then(|m| m.reply_to)
            && !hit_ids.contains(&parent)
            && by_id.contains_key(&parent)
        {
            added.insert(parent, "replied_by_hit");
        }
    }

    // An album is deleted whole.
    let hit_groups: HashSet<i64> = hits
        .iter()
        .filter_map(|h| by_id.get(&h.msg_id).and_then(|m| m.grouped_id))
        .collect();
    if !hit_groups.is_empty() {
        for m in msgs {
            if !hit_ids.contains(&m.msg_id) && m.grouped_id.is_some_and(|g| hit_groups.contains(&g))
            {
                added.insert(m.msg_id, "same_album");
            }
        }
    }

    let n = added.len();
    for (msg_id, rule) in added {
        let Some(m) = by_id.get(&msg_id) else {
            continue;
        };
        let tier = Tier::of(m.date, now);
        hits.push(Hit {
            dialog_id: m.dialog_id,
            msg_id,
            axis: Axis::Politics,
            rule_id: rule.to_string(),
            term: rule.to_string(),
            surface: None,
            layer: Layer::Structural,
            tier,
            // Weaker than a dictionary hit: it only pulls the message into the
            // window, the verdict stays with the triage.
            priority: 0.5 * tier.multiplier(),
            protected: false,
        });
    }
    n
}

/// Summary for calibrating the dictionary.
pub fn report(db: &Db) -> Result<String> {
    let mut out = String::new();

    let total: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
    let hit_msgs: i64 = db.conn.query_row(
        "SELECT COUNT(DISTINCT dialog_id || ':' || msg_id) FROM hits",
        [],
        |r| r.get(0),
    )?;

    out.push_str(&format!("messages in corpus : {total}\n"));
    out.push_str(&format!("marked             : {hit_msgs}"));
    if total > 0 {
        out.push_str(&format!(
            " ({:.2}%)",
            hit_msgs as f64 / total as f64 * 100.0
        ));
    }
    out.push_str("\n\nby axis and tier:\n");

    let mut stmt = db.conn.prepare(
        "SELECT axis, tier, COUNT(*) n, COUNT(DISTINCT dialog_id || ':' || msg_id) m
         FROM hits GROUP BY axis, tier ORDER BY axis, tier",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    for row in rows {
        let (axis, tier, n, m) = row?;
        out.push_str(&format!(
            "  {axis:<14} {tier}  hits {n:>7}  messages {m:>7}\n"
        ));
    }

    out.push_str("\ntop rules:\n");
    let mut stmt = db.conn.prepare(
        "SELECT rule_id, term, COUNT(*) n FROM hits
         GROUP BY rule_id, term ORDER BY n DESC LIMIT 25",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (rule, term, n) = row?;
        out.push_str(&format!("  {n:>7}  {rule:<20} {term}\n"));
    }

    out.push_str("\ntop dialogs by density:\n");
    let mut stmt = db.conn.prepare(
        "SELECT d.title, COUNT(DISTINCT h.msg_id) h, d.msg_count
         FROM hits h JOIN dialogs d ON d.id = h.dialog_id
         GROUP BY h.dialog_id ORDER BY h DESC LIMIT 20",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (title, h, total) = row?;
        let pct = if total > 0 {
            h as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        out.push_str(&format!("  {h:>6} / {total:<7} {pct:>5.1}%  {title}\n"));
    }

    let prot: i64 =
        db.conn
            .query_row("SELECT COUNT(*) FROM hits WHERE protected = 1", [], |r| {
                r.get(0)
            })?;
    out.push_str(&format!("\nheld by the whitelist: {prot}\n"));

    Ok(out)
}
