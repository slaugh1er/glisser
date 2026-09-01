//! Triage: window → model verdict → ranges to delete. The raw answer is
//! stored whole and `plan` is a separate command — triage decides nothing
//! for good.

use anyhow::Result;
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::db::Db;
use crate::llm::{Llm, Reply};
use crate::prompt::{self, Prompt};
use crate::window::Window;

fn schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["delete_ranges", "protected", "need_context", "notes"],
        "properties": {
            "delete_ranges": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["from", "to", "axis", "confidence", "reason"],
                    "properties": {
                        "from": {"type": "integer"},
                        "to": {"type": "integer"},
                        "axis": {"type": "string"},
                        "confidence": {"type": "number"},
                        "reason": {"type": "string"}
                    }
                }
            },
            "protected": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["from", "to", "reason"],
                    "properties": {
                        "from": {"type": "integer"},
                        "to": {"type": "integer"},
                        "reason": {"type": "string"}
                    }
                }
            },
            "need_context": {
                "type": "object",
                "additionalProperties": false,
                "required": ["before", "after", "why"],
                "properties": {
                    "before": {"type": "integer"},
                    "after": {"type": "integer"},
                    "why": {"type": "string"}
                }
            },
            "notes": {"type": "string"}
        }
    })
}

#[derive(Debug, Default, Deserialize)]
pub struct Verdict {
    #[serde(default)]
    pub delete_ranges: Vec<Range>,
    #[serde(default)]
    pub protected: Vec<Range>,
    /// Request for more context: cheaper to show everyone a small window and
    /// widen it for those who ask than to show everyone a big one.
    #[serde(default)]
    pub need_context: Option<Need>,
    /// Free-form comment, read by a human while reviewing the plan.
    #[serde(default)]
    #[allow(dead_code)]
    pub notes: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct Need {
    #[serde(default)]
    pub before: i64,
    #[serde(default)]
    pub after: i64,
    #[serde(default)]
    #[allow(dead_code)]
    pub why: String,
}

#[derive(Debug, Deserialize)]
pub struct Range {
    /// i64, not i32: the model occasionally returns junk like 764667000000
    /// and the whole run used to die on parsing. Clamped to the window.
    pub from: i64,
    pub to: i64,
    #[serde(default)]
    pub axis: String,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub reason: String,
}

struct Line {
    msg_id: i32,
    date: i64,
    outgoing: bool,
    text: String,
    media_type: Option<String>,
    transcript: Option<String>,
    marks: Option<String>,
    protected: bool,
}

/// The window as the model sees it: dense, id first, so that the answer
/// comes back as ranges rather than a retelling.
pub fn render(db: &Db, w: &Window) -> Result<String> {
    let t = prompt::text();

    let title: String = db.conn.query_row(
        "SELECT COALESCE(title, ?2) FROM dialogs WHERE id = ?1",
        params![w.dialog_id, t.untitled],
        |r| r.get(0),
    )?;

    let mut stmt = db.conn.prepare_cached(
        "SELECT m.msg_id, m.date, m.outgoing, m.text, m.media_type,
                t.text AS transcript,
                GROUP_CONCAT(DISTINCT h.axis || '/' || h.rule_id),
                m.protected
         FROM messages m
         LEFT JOIN hits h       ON h.dialog_id = m.dialog_id AND h.msg_id = m.msg_id
         LEFT JOIN transcripts t ON t.dialog_id = m.dialog_id AND t.msg_id = m.msg_id
         WHERE m.dialog_id = ?1 AND m.msg_id BETWEEN ?2 AND ?3
         GROUP BY m.msg_id
         ORDER BY m.msg_id",
    )?;

    let lines: Vec<Line> = stmt
        .query_map(params![w.dialog_id, w.from_id, w.to_id], |r| {
            Ok(Line {
                msg_id: r.get(0)?,
                date: r.get(1)?,
                outgoing: r.get::<_, i32>(2)? != 0,
                text: r.get(3)?,
                media_type: r.get(4)?,
                transcript: r.get(5)?,
                marks: r.get(6)?,
                protected: r.get::<_, i64>(7)? == 1,
            })
        })?
        .collect::<Result<_, _>>()?;

    let mut out = String::new();
    out.push_str(&format!("{}: {title}\n", t.chat));
    out.push_str(&format!("{}: {}\n", t.tier, t.tiers[w.tier as usize]));
    out.push_str(&format!(
        "{}: {}\n\n",
        t.axes,
        w.axes.iter().cloned().collect::<Vec<_>>().join(", ")
    ));
    out.push_str(t.log);
    out.push('\n');

    for l in &lines {
        let who = if l.outgoing { t.me } else { t.them };
        let date = chrono::DateTime::from_timestamp(l.date, 0)
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();

        let body = match (&l.transcript, &l.media_type, l.text.is_empty()) {
            (Some(x), _, _) => format!("[{}] {x}", t.voice),
            (None, Some(m), true) => format!("[{m}, {}]", t.no_text),
            _ => l.text.replace('\n', " ⏎ "),
        };

        out.push_str(&format!("[{}] {date} {who}: {body}\n", l.msg_id));
        if l.protected {
            out.push_str(t.protected);
            out.push('\n');
        } else if let Some(m) = &l.marks {
            out.push_str(&format!("{}{m}\n", t.fired));
        }
    }

    out.push_str(t.ask);
    Ok(out)
}

#[derive(Default)]
pub struct Stats {
    pub windows: usize,
    pub ranges: usize,
    /// Windows no verdict could be got for. The run does not fail — they are
    /// picked up by running it again.
    pub failed: usize,
    /// How many windows the model asked to widen — a direct measure of
    /// whether the base window is big enough.
    pub expanded: usize,
    pub cost: f64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

impl Stats {
    fn merge(&mut self, o: Stats) {
        self.windows += o.windows;
        self.ranges += o.ranges;
        self.failed += o.failed;
        self.expanded += o.expanded;
        self.cost += o.cost;
        self.prompt_tokens += o.prompt_tokens;
        self.completion_tokens += o.completion_tokens;
    }
}

/// A run over a ready set of windows, chosen outside (`window::select`) so
/// that different models see the same one. Windows share no state, so threads
/// take them from one counter; the time goes into the network, not the CPU,
/// which is why `jobs` is set well above the core count.
pub fn run(db: &Db, llm: &Llm, prompt: &Prompt, windows: &[Window], jobs: usize) -> Result<Stats> {
    let schema = schema();
    let next = AtomicUsize::new(0);
    let seen = AtomicUsize::new(0);
    // Micro-dollars: the atomic exists only to print live progress, the total
    // is summed from the exact per-thread f64.
    let spent = AtomicU64::new(0);
    // Threads get the path, not the `Db`: `Connection` is not Send.
    let path = db.path.clone();

    let parts: Vec<Result<Stats>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..jobs.max(1))
            .map(|_| {
                s.spawn(|| -> Result<Stats> {
                    // One connection per thread: everyone reads in parallel,
                    // WAL takes one writer at a time, one verdict per write.
                    let db = Db::open(&path)?;
                    let mut st = Stats::default();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(w) = windows.get(i) else { break };
                        if !done(&db, w, &llm.model, &prompt.id)? {
                            let was = st.cost;
                            one(&db, llm, prompt, &schema, w, &mut st)?;
                            spent.fetch_add(((st.cost - was) * 1e6) as u64, Ordering::Relaxed);
                        }
                        let n = seen.fetch_add(1, Ordering::Relaxed) + 1;
                        if n.is_multiple_of(25) {
                            tracing::info!(
                                "done {}/{}, ${:.3}",
                                n,
                                windows.len(),
                                spent.load(Ordering::Relaxed) as f64 / 1e6,
                            );
                        }
                    }
                    Ok(st)
                })
            })
            .collect();

        hs.into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("a triage thread died")))
            })
            .collect()
    });

    let mut st = Stats::default();
    for p in parts {
        st.merge(p?);
    }
    Ok(st)
}

/// One window: the call, the optional widening, the stored verdict.
fn one(
    db: &Db,
    llm: &Llm,
    prompt: &Prompt,
    schema: &Value,
    w: &Window,
    st: &mut Stats,
) -> Result<()> {
    // Widened only if the model says the window is not enough, and once
    // only: a second widening costs more than a big window would have.
    let mut cur = w.clone();
    let (verdict, reply, passes) = loop {
        let user = render(db, &cur)?;
        // One refused window must not bring down a run of a thousand calls.
        let reply = match llm.complete(&prompt.text, &user, schema) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "window {}:{}..{} would not give a verdict ({e:#}), skipping",
                    cur.dialog_id,
                    cur.from_id,
                    cur.to_id
                );
                st.failed += 1;
                break (Verdict::default(), Reply::default(), 0);
            }
        };
        st.cost += reply.cost;
        st.prompt_tokens += reply.prompt_tokens;
        st.completion_tokens += reply.completion_tokens;

        let v: Verdict = match serde_json::from_str(&reply.content) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "window {}:{}..{} — invalid verdict ({e}), skipping",
                    cur.dialog_id,
                    cur.from_id,
                    cur.to_id
                );
                break (Verdict::default(), reply, 0);
            }
        };

        let need = v
            .need_context
            .as_ref()
            .filter(|n| n.before > 0 || n.after > 0);
        match need {
            Some(n) if cur.from_id == w.from_id && cur.to_id == w.to_id => {
                let wider = crate::window::expand(db, &cur, n.before, n.after)?;
                if wider.total > cur.total {
                    st.expanded += 1;
                    cur = wider;
                    continue;
                }
                break (v, reply, 1);
            }
            _ => break (v, reply, if cur.total > w.total { 2 } else { 1 }),
        }
    };

    // passes == 0 — no verdict; nothing is stored, so a repeat run gives this
    // window an honest second try.
    if passes == 0 {
        if !reply.content.is_empty() {
            st.failed += 1;
        }
        return Ok(());
    }
    store(db, w, &llm.model, &prompt.id, &reply, passes)?;
    st.ranges += verdict.delete_ranges.len();
    st.windows += 1;
    Ok(())
}

fn done(db: &Db, w: &Window, model: &str, prompt_id: &str) -> Result<bool> {
    let n: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM triage_runs
         WHERE dialog_id=?1 AND window_from=?2 AND window_to=?3
           AND model=?4 AND prompt_id=?5",
        params![w.dialog_id, w.from_id, w.to_id, model, prompt_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Keyed by the original window, not the widened one: otherwise runs of
/// different models would drift apart and there would be nothing to compare.
fn store(
    db: &Db,
    w: &Window,
    model: &str,
    prompt_id: &str,
    reply: &Reply,
    passes: i64,
) -> Result<()> {
    db.conn.execute(
        "INSERT OR REPLACE INTO triage_runs
         (dialog_id, window_from, window_to, model, prompt_id, raw,
          prompt_tokens, completion_tokens, cost, passes, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            w.dialog_id,
            w.from_id,
            w.to_id,
            model,
            prompt_id,
            reply.content,
            reply.prompt_tokens as i64,
            reply.completion_tokens as i64,
            reply.cost,
            passes,
            chrono::Utc::now().timestamp(),
        ],
    )?;
    Ok(())
}

/// The manual gate: `purge` takes only what is approved. A separate step, so
/// that rebuilding the plan cannot carry an old approval over.
pub fn approve(db: &Db, dialog: Option<i64>, axis: Option<&str>, on: bool) -> Result<usize> {
    let n = db.conn.execute(
        "UPDATE plan SET approved = ?1
         WHERE (?2 IS NULL OR dialog_id = ?2) AND (?3 IS NULL OR axis = ?3)",
        params![on as i64, dialog, axis],
    )?;
    Ok(n)
}

/// Rebuild `plan` from the verdicts of one run — a separate step so that a
/// bake-off cannot mix verdicts. The last safety catch lives here: protected
/// messages never enter the plan whatever the model answered, and a range is
/// clamped to the window the model actually saw.
pub fn rebuild_plan(db: &Db, model: &str, prompt_id: Option<&str>) -> Result<usize> {
    db.conn.execute("DELETE FROM plan", [])?;

    let mut stmt = db.conn.prepare(
        "SELECT dialog_id, window_from, window_to, raw FROM triage_runs
         WHERE model = ?1 AND (?2 IS NULL OR prompt_id = ?2)",
    )?;
    let rows: Vec<(i64, i32, i32, String)> = stmt
        .query_map(params![model, prompt_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<_, _>>()?;

    if rows.is_empty() {
        anyhow::bail!("no runs for model `{model}` — triage first");
    }

    let mut n = 0;
    for (dialog_id, from_id, to_id, raw) in rows {
        let v: Verdict = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("unparsed verdict {dialog_id}:{from_id}: {e}");
                continue;
            }
        };
        for r in &v.delete_ranges {
            let (lo, hi) = if r.from <= r.to {
                (r.from, r.to)
            } else {
                (r.to, r.from)
            };
            let (lo, hi) = (
                lo.clamp(from_id as i64, to_id as i64) as i32,
                hi.clamp(from_id as i64, to_id as i64) as i32,
            );
            if lo > hi {
                continue;
            }
            n += db.conn.execute(
                "INSERT OR IGNORE INTO plan (dialog_id, msg_id, reason, axis, confidence)
                 SELECT m.dialog_id, m.msg_id, ?4, ?5, ?6
                 FROM messages m
                 WHERE m.dialog_id = ?1 AND m.msg_id BETWEEN ?2 AND ?3
                   AND m.protected = 0",
                params![dialog_id, lo, hi, r.reason, r.axis, r.confidence],
            )?;
        }
    }
    Ok(n)
}
