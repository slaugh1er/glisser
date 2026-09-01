//! glisser — curating one's own Telegram history before a border crossing.
//!
//! `export` → `scan` → `triage` → `eval` → `plan` → `approve` → `purge.py`.
//! Modes are independent, all state is SQLite, everything lives in `state/`.

mod audit;
mod config;
mod db;
mod dict;
mod diff;
mod eval;
mod ingest;
mod llm;
mod model;
mod norm;
mod prompt;
mod scan;
mod triage;
mod window;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Printed at every start. Raw string: the art is full of backslashes.
const BANNER: &str = r#"
         | _)
   _` |  |  |   __|   __|   _ \   __|
  (   |  |  | \__ \ \__ \   __/  |
 \__, | _| _| ____/ ____/ \___| _|
 |___/
"#;

#[derive(Parser)]
#[command(name = "glisser", version, about, long_about = None)]
struct Cli {
    /// Database. Keep it on an encrypted volume that does not travel.
    #[arg(long, default_value = "state/glisser.db", global = true)]
    db: PathBuf,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Load the corpus from a Telegram Desktop export (result.json). No API call.
    Export {
        #[arg(default_value = "state/telegram_dump/result.json")]
        path: PathBuf,
        /// Your own user_id, if the export does not reveal it.
        #[arg(long)]
        me: Option<i64>,
    },

    /// Find and mark the risky messages. Idempotent, run it as often as needed.
    Scan {
        /// One axis only: politics|crypto|crypto_hard|foreign_media|emigration|lgbt|foreign_finance
        #[arg(long)]
        axis: Option<String>,
        /// Keep the previous marks instead of clearing them first.
        #[arg(long)]
        keep: bool,
    },

    /// Corpus and detection figures, for calibrating the dictionary.
    Stats,

    /// Build the triage windows and show what they will cost in LLM calls.
    Windows {
        /// Only windows newer than this date (YYYY-MM-DD) — a repeat run.
        #[arg(long)]
        since: Option<String>,
        /// A rule the dictionary did not have last time: its windows are
        /// taken regardless of `--since`. Repeatable.
        #[arg(long = "new-rule")]
        new_rule: Vec<String>,
    },

    /// Run the windows through the LLM and get ranges to delete. Decides
    /// nothing for good: verdicts are stored and the plan is assembled
    /// separately, so a bake-off of models spoils nothing.
    Triage {
        /// OpenRouter model. A second run with another one gives `eval`
        /// a cross-check.
        #[arg(long, default_value = "anthropic/claude-sonnet-5")]
        model: String,
        /// The first N windows — the freshest and sharpest.
        #[arg(long)]
        limit: Option<usize>,
        /// N windows stratified by axis. The set is deterministic: this is
        /// the flag for running different models over the same material.
        #[arg(long)]
        sample: Option<usize>,
        /// How many windows to keep in flight. Bound by the network, not
        /// the CPU, so more than the core count is normal.
        #[arg(long, default_value_t = 8)]
        jobs: usize,
        /// Call nothing: show the window as the model would see it.
        #[arg(long)]
        dry_run: bool,
        /// Only windows newer than this date (YYYY-MM-DD). On a repeat run
        /// there is no reason to pay twice for what was already judged.
        #[arg(long)]
        since: Option<String>,
        /// A rule the dictionary did not have last time: its windows are
        /// taken regardless of `--since`. Repeatable.
        #[arg(long = "new-rule")]
        new_rule: Vec<String>,
    },

    /// Metrics per run: how much was cut, whitelist violations, anchor
    /// coverage; plus model agreement and the windows queued for a human.
    Eval,

    /// Assemble `plan` from the verdicts of one run. Rebuildable at will —
    /// the only input `purge` has.
    Plan {
        #[arg(long)]
        model: String,
        /// Policy version; without it, every version of that model is taken.
        #[arg(long)]
        prompt: Option<String>,
    },

    /// Mark plan rows as reviewed. `purge` deletes only those; rebuilding
    /// the plan clears the approval, deliberately.
    Approve {
        #[arg(long)]
        dialog: Option<i64>,
        #[arg(long)]
        axis: Option<String>,
        /// Approve the whole plan. Required explicitly, so that it cannot
        /// happen by missing a filter.
        #[arg(long)]
        all: bool,
        /// Revoke the approval instead of setting it.
        #[arg(long)]
        revoke: bool,
    },

    /// The difference between two runs: the common area and the tails. The
    /// common area is trusted; only the tails need a human.
    Diff {
        #[arg(long)]
        base: String,
        #[arg(long)]
        against: String,
        /// How many reason groups to print, with examples.
        #[arg(long, default_value_t = 10)]
        show: usize,
    },

    /// Audit the dictionary: where it is noisy and what to clean out.
    Audit {
        #[command(subcommand)]
        what: AuditCmd,
    },

    /// Check the dictionary and the normalization on one string, no database.
    Probe { text: String },
}

#[derive(Subcommand)]
enum AuditCmd {
    /// Frequency slice over terms. Hits concentrated in one dialog point to
    /// a word boundary or professional jargon.
    Terms {
        /// Hide terms with fewer than N hits.
        #[arg(long, default_value_t = 5)]
        min: usize,
        /// How many random samples to print under each term.
        #[arg(long, default_value_t = 0)]
        samples: usize,
        /// Only terms, rules or axes containing this substring.
        #[arg(long)]
        filter: Option<String>,
    },
    /// How many of each rule's hits the model actually cut. Computed from
    /// calls already made, needs no new ones.
    Rules {
        #[arg(long)]
        model: Option<String>,
    },
}

fn main() -> Result<()> {
    println!("{BANNER}");

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .without_time()
        .init();

    // The OpenRouter key lives with the rest of the state, so that one gesture
    // destroys it. A key already in the environment still wins.
    let _ = dotenvy::from_filename("state/.env");

    let cli = Cli::parse();

    // SQLite silently creates a missing file, so a run from the wrong
    // directory used to report zero messages instead of failing. Only the
    // ingest is allowed to create the database; `probe` never opens it.
    if !matches!(cli.cmd, Cmd::Export { .. } | Cmd::Probe { .. }) && !cli.db.exists() {
        anyhow::bail!(
            "no database at {} — looks like the wrong directory (set it with --db)",
            cli.db.display()
        );
    }

    match cli.cmd {
        Cmd::Export { path, me } => {
            let mut db = db::Db::open(&cli.db)?;
            let s = ingest::export::ingest(&mut db, &path, me)?;
            println!("dialogs    : {}", s.dialogs);
            println!("messages   : {}", s.messages);
            println!("service    : {} (skipped)", s.skipped_service);
            match s.my_id {
                Some(id) => println!("own id     : {id}"),
                None => println!("own id     : unknown — pass --me"),
            }
        }

        Cmd::Scan { axis, keep } => {
            let dict = dict::Dictionary::new()?;
            let mut db = db::Db::open(&cli.db)?;
            if !keep {
                db.clear_hits()?;
            }
            let only = axis.as_deref().map(parse_axis).transpose()?;

            println!(
                "dictionary: {} terms, {} regexes",
                dict.term_count(),
                dict.regex_count()
            );

            let s = scan::run(&mut db, &dict, only)?;
            println!("dialogs       : {}", s.dialogs);
            println!("messages      : {}", s.messages);
            println!("hits          : {}", s.hits);
            println!("structural    : {}", s.structural);
            println!("protected     : {}", s.protected);
        }

        Cmd::Stats => {
            let db = db::Db::open(&cli.db)?;
            print!("{}", scan::report(&db)?);
        }

        Cmd::Windows { since, new_rule } => {
            let db = db::Db::open(&cli.db)?;
            let cut = cutoff(since.as_deref(), new_rule)?;
            let w = window::select(window::build(&db)?, None, None, cut.as_ref());
            print!("{}", window::report(&w));
        }

        Cmd::Triage {
            model,
            limit,
            sample,
            jobs,
            dry_run,
            since,
            new_rule,
        } => {
            let db = db::Db::open(&cli.db)?;
            let p = prompt::build();
            let cut = cutoff(since.as_deref(), new_rule)?;
            let picked = window::select(
                window::build(&db)?,
                limit.or(if dry_run { Some(1) } else { None }),
                sample,
                cut.as_ref(),
            );

            if dry_run {
                for win in &picked {
                    println!("{}\n{}\n", "=".repeat(70), triage::render(&db, win)?);
                }
                println!(
                    "policy {} , windows picked {}  (no calls made)",
                    p.id,
                    picked.len()
                );
            } else {
                let llm = llm::Llm::new(&model)?;
                println!(
                    "policy {} , windows {} , threads {jobs}",
                    p.id,
                    picked.len()
                );
                let s = triage::run(&db, &llm, &p, &picked, jobs)?;
                println!("windows done    : {}", s.windows);
                println!("failed          : {}", s.failed);
                println!("ranges          : {}", s.ranges);
                println!("asked to widen  : {} windows", s.expanded);
                println!(
                    "tokens          : {} / {}",
                    s.prompt_tokens, s.completion_tokens
                );
                println!("cost            : ${:.4}", s.cost);
            }
        }

        Cmd::Eval => {
            let db = db::Db::open(&cli.db)?;
            print!("{}", eval::report(&db)?);
        }

        Cmd::Plan { model, prompt } => {
            let db = db::Db::open(&cli.db)?;
            let n = triage::rebuild_plan(&db, &model, prompt.as_deref())?;
            println!("in the plan: {n} messages");
        }

        Cmd::Approve {
            dialog,
            axis,
            all,
            revoke,
        } => {
            if dialog.is_none() && axis.is_none() && !all {
                anyhow::bail!("pass --dialog or --axis, or --all for the whole plan");
            }
            let db = db::Db::open(&cli.db)?;
            let n = triage::approve(&db, dialog, axis.as_deref(), !revoke)?;
            let word = if revoke { "revoked from" } else { "approved" };
            println!("{word} {n} messages");
        }

        Cmd::Diff {
            base,
            against,
            show,
        } => {
            let db = db::Db::open(&cli.db)?;
            print!("{}", diff::report(&db, &base, &against, show)?);
        }

        Cmd::Audit { what } => {
            let db = db::Db::open(&cli.db)?;
            match what {
                AuditCmd::Terms {
                    min,
                    samples,
                    filter,
                } => {
                    print!("{}", audit::terms(&db, min, samples, filter.as_deref())?);
                }
                AuditCmd::Rules { model } => {
                    print!("{}", audit::rules(&db, model.as_deref())?);
                }
            }
        }

        Cmd::Probe { text } => {
            let dict = dict::Dictionary::new()?;
            let n = norm::Normalized::build(&text);
            println!("normalized: {}", n.text);
            println!("protected : {}", dict.is_protected(&n));
            for m in dict.scan(&text, &n) {
                println!(
                    "  [{}] {} `{}` weight {:.2}  {}",
                    m.axis.as_str(),
                    m.rule_id,
                    m.term,
                    m.weight,
                    m.surface.as_deref().unwrap_or("")
                );
            }
        }
    }

    Ok(())
}

/// Cutoff for a repeat run. Without `--since` there is no filter at all:
/// `--new-rule` alone would mean «look only at the new rules», which is not
/// the question the flag exists for.
fn cutoff(since: Option<&str>, new_rules: Vec<String>) -> Result<Option<window::Cutoff>> {
    let Some(s) = since else {
        if !new_rules.is_empty() {
            anyhow::bail!("--new-rule only works together with --since");
        }
        return Ok(None);
    };
    let d = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|e| anyhow::anyhow!("--since expects a date like 2026-08-10: {e}"))?;
    Ok(Some(window::Cutoff {
        since: d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(),
        new_rules: new_rules.into_iter().collect(),
    }))
}

fn parse_axis(s: &str) -> Result<model::Axis> {
    model::Axis::parse(s).ok_or_else(|| anyhow::anyhow!("unknown axis: {s}"))
}
