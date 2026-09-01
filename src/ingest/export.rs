//! Ingest from a Telegram Desktop export (Settings → Advanced → Export
//! Telegram data, JSON). Not one API call. What it structurally lacks —
//! `access_hash`, `grouped_id` — comes over MTProto from `purge/pull.py`.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

use crate::model::{Dialog, Message, PeerKind};

#[derive(Debug, Deserialize)]
struct Export {
    #[serde(default)]
    personal_information: Option<PersonalInfo>,
    #[serde(default)]
    chats: Option<Chats>,
    // A single-chat export is a Chat with no wrapper.
    #[serde(flatten)]
    single: Option<Chat>,
}

#[derive(Debug, Deserialize)]
struct PersonalInfo {
    #[serde(default)]
    user_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct Chats {
    #[serde(default)]
    list: Vec<Chat>,
}

#[derive(Debug, Deserialize)]
struct Chat {
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    messages: Vec<RawMessage>,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    id: i32,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    /// A string in the export, not a number.
    #[serde(default)]
    date_unixtime: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    from_id: Option<String>,
    #[serde(default)]
    reply_to_message_id: Option<i32>,
    #[serde(default)]
    forwarded_from: Option<serde_json::Value>,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    photo: Option<String>,
    /// Either a string or an array of strings and entity objects.
    #[serde(default)]
    text: serde_json::Value,
}

pub struct Stats {
    pub dialogs: usize,
    pub messages: usize,
    pub skipped_service: usize,
    pub my_id: Option<i64>,
}

/// Parse `result.json` and load the corpus into the database.
pub fn ingest(db: &mut crate::db::Db, path: &Path, me_override: Option<i64>) -> Result<Stats> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("no export file at {}", path.display()))?;
    let size_mb = meta.len() / 1_048_576;
    if size_mb > 1024 {
        tracing::warn!(
            "the export is {} MB and is parsed entirely in memory",
            size_mb
        );
    }

    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::with_capacity(1 << 20, file);
    let export: Export =
        serde_json::from_reader(reader).context("could not parse the export JSON")?;

    let chats: Vec<Chat> = match (export.chats, export.single) {
        (Some(c), _) => c.list,
        (None, Some(single)) if single.id.is_some() => vec![single],
        _ => bail!("neither `chats.list` nor a single chat in the file — is it a Telegram export?"),
    };

    let my_id = me_override
        .or_else(|| export.personal_information.as_ref().and_then(|p| p.user_id))
        .or_else(|| infer_my_id(&chats));

    if my_id.is_none() {
        tracing::warn!(
            "could not work out your own user_id — every message is marked incoming. \
             Pass --me <id>, or the mine/theirs figures in the plan will be wrong"
        );
    }

    let mut stats = Stats {
        dialogs: 0,
        messages: 0,
        skipped_service: 0,
        my_id,
    };

    for chat in &chats {
        let Some(id) = chat.id else { continue };
        let kind = PeerKind::from_export_type(chat.kind.as_deref().unwrap_or(""));

        let mut msgs = Vec::with_capacity(chat.messages.len());
        for raw in &chat.messages {
            // Service messages (joined a group, changed a photo) carry no
            // text and are not deleted like ordinary ones.
            if raw.kind.as_deref() == Some("service") {
                stats.skipped_service += 1;
                continue;
            }

            let date = parse_date(raw)?;
            let from_id = raw.from_id.as_deref().and_then(parse_peer_id);
            let outgoing = match (my_id, from_id) {
                (Some(me), Some(f)) => f == me,
                _ => false,
            };

            msgs.push(Message {
                dialog_id: id,
                msg_id: raw.id,
                date,
                from_id,
                outgoing,
                reply_to: raw.reply_to_message_id,
                // The export has no grouped_id.
                grouped_id: None,
                fwd_from: raw.forwarded_from.as_ref().and_then(value_to_string),
                media_type: media_type(raw),
                file_path: raw.file.clone().or_else(|| raw.photo.clone()),
                text: flatten_text(&raw.text),
            });
        }

        db.upsert_dialog(&Dialog {
            id,
            kind,
            access_hash: None,
            title: chat.name.clone().unwrap_or_else(|| format!("id{id}")),
            username: None,
            archived: false,
            msg_count: msgs.len() as i64,
        })?;

        stats.messages += db.insert_messages(&msgs)?;
        stats.dialogs += 1;
    }

    Ok(stats)
}

/// In a personal chat `chat.id` is the other person, so any other sender in
/// it is me. Works even when `personal_information` gave no id.
fn infer_my_id(chats: &[Chat]) -> Option<i64> {
    // Saved Messages is the surest sign: its id is my own user_id.
    for c in chats {
        if c.kind.as_deref() == Some("saved_messages")
            && let Some(id) = c.id
        {
            return Some(id);
        }
    }

    // Otherwise, a vote over the personal chats.
    let mut votes: HashMap<i64, usize> = HashMap::new();
    for c in chats {
        if c.kind.as_deref() != Some("personal_chat") {
            continue;
        }
        let Some(peer) = c.id else { continue };
        for m in &c.messages {
            if let Some(f) = m.from_id.as_deref().and_then(parse_peer_id)
                && f != peer
            {
                *votes.entry(f).or_default() += 1;
            }
        }
    }
    votes.into_iter().max_by_key(|(_, n)| *n).map(|(id, _)| id)
}

/// `from_id` in the export is a string like `user123456` or `channel123456`.
fn parse_peer_id(s: &str) -> Option<i64> {
    let digits = s.trim_start_matches(|c: char| !c.is_ascii_digit() && c != '-');
    digits.parse().ok()
}

fn parse_date(raw: &RawMessage) -> Result<i64> {
    if let Some(u) = &raw.date_unixtime
        && let Ok(v) = u.parse::<i64>()
    {
        return Ok(v);
    }
    if let Some(d) = &raw.date {
        // ISO 8601 without a zone: the export writes local time.
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(d, "%Y-%m-%dT%H:%M:%S") {
            return Ok(dt.and_utc().timestamp());
        }
    }
    // Without a date there is no tier, and the tier shapes the verdict.
    bail!("message {} has no parsable date", raw.id)
}

fn media_type(raw: &RawMessage) -> Option<String> {
    if let Some(mt) = &raw.media_type {
        return Some(mt.clone());
    }
    if raw.photo.is_some() {
        return Some("photo".to_string());
    }
    if raw.file.is_some() {
        return Some(raw.mime_type.clone().unwrap_or_else(|| "file".to_string()));
    }
    None
}

fn value_to_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
        serde_json::Value::Null | serde_json::Value::Bool(false) => None,
        other => Some(other.to_string()),
    }
}

/// `text` is either a string or an array of strings and entities (links,
/// mentions, code). Flattened: entities carry meaningful words.
fn flatten_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => {
            let mut out = String::new();
            for it in items {
                match it {
                    serde_json::Value::String(s) => out.push_str(s),
                    serde_json::Value::Object(o) => {
                        if let Some(serde_json::Value::String(t)) = o.get("text") {
                            out.push_str(t);
                        }
                        // For links the href can be the meaningful part.
                        if let Some(serde_json::Value::String(h)) = o.get("href") {
                            out.push(' ');
                            out.push_str(h);
                        }
                    }
                    _ => {}
                }
            }
            out
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_flattens_string_and_entity_array() {
        assert_eq!(flatten_text(&json!("hello")), "hello");

        let v = json!([
            "look ",
            {"type": "text_link", "text": "here", "href": "https://example.com/x"},
            " done"
        ]);
        let out = flatten_text(&v);
        assert!(out.contains("look"));
        assert!(out.contains("here"));
        // The href carries the domain — a signal in its own right.
        assert!(out.contains("example.com"));
        assert!(out.contains("done"));
    }

    #[test]
    fn peer_ids_parse_from_prefixed_form() {
        assert_eq!(parse_peer_id("user123456"), Some(123456));
        assert_eq!(parse_peer_id("channel987"), Some(987));
        assert_eq!(parse_peer_id("junk"), None);
    }

    #[test]
    fn saved_messages_id_identifies_me() {
        let chats = vec![Chat {
            name: Some("Saved".into()),
            kind: Some("saved_messages".into()),
            id: Some(555),
            messages: vec![],
        }];
        assert_eq!(infer_my_id(&chats), Some(555));
    }

    #[test]
    fn my_id_inferred_from_personal_chat_when_no_saved_messages() {
        // In a chat with 777, anything not from 777 is from me.
        let chats = vec![Chat {
            name: Some("Friend".into()),
            kind: Some("personal_chat".into()),
            id: Some(777),
            messages: vec![
                RawMessage {
                    id: 1,
                    kind: None,
                    date_unixtime: Some("1".into()),
                    date: None,
                    from_id: Some("user777".into()),
                    reply_to_message_id: None,
                    forwarded_from: None,
                    media_type: None,
                    mime_type: None,
                    file: None,
                    photo: None,
                    text: json!(""),
                },
                RawMessage {
                    id: 2,
                    kind: None,
                    date_unixtime: Some("2".into()),
                    date: None,
                    from_id: Some("user42".into()),
                    reply_to_message_id: None,
                    forwarded_from: None,
                    media_type: None,
                    mime_type: None,
                    file: None,
                    photo: None,
                    text: json!(""),
                },
            ],
        }];
        assert_eq!(infer_my_id(&chats), Some(42));
    }

    #[test]
    fn unixtime_string_is_parsed() {
        let raw = RawMessage {
            id: 1,
            kind: None,
            date_unixtime: Some("1577836800".into()),
            date: None,
            from_id: None,
            reply_to_message_id: None,
            forwarded_from: None,
            media_type: None,
            mime_type: None,
            file: None,
            photo: None,
            text: json!(""),
        };
        assert_eq!(parse_date(&raw).unwrap(), 1_577_836_800);
    }
}
