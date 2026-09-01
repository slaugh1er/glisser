//! Domain types shared by every stage of the pipeline.

use crate::config;

/// Peer type. Decides what can be done with a dialog at all: a broadcast
/// channel holds no messages of ours — only unsubscribing removes us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerKind {
    User,
    Chat,
    Channel,
    Megagroup,
    SavedMessages,
}

impl PeerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Chat => "chat",
            Self::Channel => "channel",
            Self::Megagroup => "megagroup",
            Self::SavedMessages => "saved_messages",
        }
    }

    /// `type` values as Telegram Desktop writes them into the export.
    pub fn from_export_type(s: &str) -> Self {
        match s {
            "saved_messages" => Self::SavedMessages,
            "personal_chat" | "bot_chat" | "replies" => Self::User,
            "private_group" => Self::Chat,
            "private_supergroup" | "public_supergroup" => Self::Megagroup,
            "private_channel" | "public_channel" => Self::Channel,
            // An unknown type is treated as a supergroup: that one is
            // deletable, so it reaches the plan and is seen by a human.
            _ => Self::Megagroup,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Dialog {
    /// Raw id, without the -100 prefix.
    pub id: i64,
    pub kind: PeerKind,
    /// Needed to build an InputPeer for deletion. Absent from the export —
    /// filled in over MTProto by `purge/pull.py`.
    pub access_hash: Option<i64>,
    pub title: String,
    pub username: Option<String>,
    pub archived: bool,
    pub msg_count: i64,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub dialog_id: i64,
    /// Server-side id inside the chat — this is what goes to `delete_messages`.
    pub msg_id: i32,
    pub date: i64,
    pub from_id: Option<i64>,
    pub outgoing: bool,
    pub reply_to: Option<i32>,
    /// Album. Absent from the export.
    pub grouped_id: Option<i64>,
    pub fwd_from: Option<String>,
    pub media_type: Option<String>,
    pub file_path: Option<String>,
    pub text: String,
}

/// Detection axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    Politics,
    Crypto,
    /// Machine artifacts: addresses, BIP39, transaction hashes.
    CryptoHard,
    ForeignMedia,
    Emigration,
    Lgbt,
    /// Accounts and cards at foreign banks. Owning one is legal, but an
    /// undeclared one is an offence — and the fact itself reads as a bolt-hole.
    ForeignFinance,
}

impl Axis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Politics => "politics",
            Self::Crypto => "crypto",
            Self::CryptoHard => "crypto_hard",
            Self::ForeignMedia => "foreign_media",
            Self::Emigration => "emigration",
            Self::Lgbt => "lgbt",
            Self::ForeignFinance => "foreign_finance",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|a| a.as_str() == s)
    }

    pub const ALL: [Axis; 7] = [
        Axis::Politics,
        Axis::Crypto,
        Axis::CryptoHard,
        Axis::ForeignMedia,
        Axis::Emigration,
        Axis::Lgbt,
        Axis::ForeignFinance,
    ];
}

/// Time tier. The same term earns a different verdict depending on its age:
/// the recent is cut hard, the old and everyday is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    T0,
    T1,
    T2,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::T0 => "T0",
            Self::T1 => "T1",
            Self::T2 => "T2",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "T0" => Some(Self::T0),
            "T1" => Some(Self::T1),
            "T2" => Some(Self::T2),
            _ => None,
        }
    }

    pub fn of(msg_date: i64, now: i64) -> Self {
        match (now - msg_date) / 86_400 {
            days if days <= config::T0_DAYS => Self::T0,
            days if days <= config::T1_DAYS => Self::T1,
            _ => Self::T2,
        }
    }

    /// Weight multiplier. Not a verdict: it only decides whether a model call
    /// is worth spending, and in what order.
    pub fn multiplier(self) -> f64 {
        match self {
            Self::T0 => config::T0_MULTIPLIER,
            Self::T1 => config::T1_MULTIPLIER,
            Self::T2 => config::T2_MULTIPLIER,
        }
    }
}

/// The point in time ages are measured from.
pub fn now() -> i64 {
    match config::REFERENCE_DATE {
        Some(d) => chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
            .expect("REFERENCE_DATE in config.rs must look like 2026-08-10")
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp(),
        None => chrono::Utc::now().timestamp(),
    }
}

/// Layer the detection fired on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Keyword,
    Regex,
    /// Dialog topology: reply, album, forward.
    Structural,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::Regex => "regex",
            Self::Structural => "structural",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub dialog_id: i64,
    pub msg_id: i32,
    pub axis: Axis,
    pub rule_id: String,
    pub term: String,
    /// How the match looked in the original text — evidence for the report.
    pub surface: Option<String>,
    pub layer: Layer,
    pub tier: Tier,
    /// Rule weight × tier multiplier.
    pub priority: f64,
    /// Whitelisted: kept despite the hit.
    pub protected: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    #[test]
    fn tier_boundaries() {
        let now = 1_000_000 * DAY;
        assert_eq!(Tier::of(now, now), Tier::T0);
        assert_eq!(Tier::of(now - 180 * DAY, now), Tier::T0);
        assert_eq!(Tier::of(now - 181 * DAY, now), Tier::T1);
        assert_eq!(Tier::of(now - 365 * DAY, now), Tier::T1);
        // Over a year old and everyday: kept.
        assert_eq!(Tier::of(now - 366 * DAY, now), Tier::T2);
    }

    #[test]
    fn older_tiers_are_damped() {
        assert!(Tier::T2.multiplier() < Tier::T1.multiplier());
        assert!(Tier::T1.multiplier() < Tier::T0.multiplier());
    }

    #[test]
    fn axis_names_round_trip() {
        for a in Axis::ALL {
            assert_eq!(Axis::parse(a.as_str()), Some(a));
        }
    }
}
