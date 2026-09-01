//! Configuration. Everything a user is expected to change lives here.
//!
//! suckless style: this file is the config — edit it and rebuild. What ships
//! is this file with an empty profile, an empty whitelist and no ignored
//! dialogs; a real one is a local change, kept out of a commit.

use crate::prompt::{Lang, Profile};

// --- owner ---------------------------------------------------------------
//
// Every field is optional and every one of them only ever *softens* the
// policy. Left as `None`, the line is simply absent from the prompt and the
// policy runs in its general, more conservative mode. Write the values in the
// language of `LANG`.

pub const PROFILE: Profile = Profile {
    // An exculpatory signal, never a reason to cut: if crypto is the owner's
    // job, their crypto talk stays. `None` — no excuse, personal ownership
    // is cut by the general rule.
    // e.g. Some("a programmer; crypto is the subject of their work")
    occupation: None,

    // Cut ties to an account in this country. `None` — cut any sign of owning
    // a foreign account at all.
    // e.g. Some("Kazakhstan")
    account_country: None,

    // The cover story: talk that confirms this trip is protected. `None` —
    // no protected country, the emigration axis works in general terms.
    // e.g. Some("China")
    travel_country: None,
};

/// Language of the policy sent to the model and of the window rendered with it.
pub const LANG: Lang = Lang::Ru;

// --- protected content ---------------------------------------------------
//
// Not «don't flag», but *keep*: talk of the trip confirms the cover story and
// works for the owner. An override term cancels the protection — explicit
// immigration vocabulary turns a holiday into «I am staying».
//
// Terms are matched against the corpus, so write them in the language the
// corpus is in — the places and words your own cover story is made of.

pub const WHITELIST: &[&str] = &[];

#[rustfmt::skip]
pub const WHITELIST_OVERRIDE: &[&str] = &[
    "внж", "вид на жительство", "остаться навсегда", "остатьсянавсегд",
    "не вернус", "невернус", "переезжаю навсегда", "переезжаюнавсегд",
    "digital nomad", "цифровой кочевник", "продать квартиру", "продатьквартир",
    "релокейт", "релокац", "эмигрир", "иммигрир",
];

// --- time tiers ----------------------------------------------------------
//
// The same term earns a different verdict depending on its age: a brother-in-
// law who lived abroad a year ago is an asset, three months ago is a risk.
// T2 is damped on purpose — old everyday talk is the naturalness that
// protects.

pub const T0_DAYS: i64 = 180;
pub const T1_DAYS: i64 = 365;
pub const T0_MULTIPLIER: f64 = 1.0;
pub const T1_MULTIPLIER: f64 = 0.85;
pub const T2_MULTIPLIER: f64 = 0.45;

/// Date the ages are counted from, `YYYY-MM-DD`. `None` — today.
pub const REFERENCE_DATE: Option<&str> = None;

// --- scan ----------------------------------------------------------------

/// ±N messages around a hit: the window that goes to the model.
pub const WINDOW: i64 = 30;
/// Hits closer than this collapse into one window, so that a dense political
/// argument costs one call instead of two hundred.
pub const CLUSTER_GAP: i64 = 30;
/// Summed weight a message needs to become an anchor.
pub const HIT_THRESHOLD: f64 = 1.0;
/// Ceiling when the model asks for more context around a window.
pub const MAX_WINDOW: i64 = 120;

/// Dialog kinds left out of triage. Bots and channels are deleted by hand in
/// seconds; model calls are expensive. Personal correspondence is the target.
pub const IGNORE_KINDS: &[&str] = &["megagroup", "chat"];

/// Individual dialogs left out of triage, by id.
pub const IGNORE_DIALOGS: &[i64] = &[];

// --- deletion budget -----------------------------------------------------
//
// Exceeding it is not an error but a signal: the plan is destructive enough
// to have become a flag of its own.

/// Share of the whole corpus.
pub const MAX_DELETE_RATIO: f64 = 0.05;
/// Share of a single dialog.
pub const MAX_DIALOG_RATIO: f64 = 0.40;
