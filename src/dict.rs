//! Dictionary and matcher. Terms are normalized the same way as the corpus
//! and folded into one Aho-Corasick automaton; regexes run on the raw text,
//! for what must not be normalized (addresses, seed phrases).

use aho_corasick::AhoCorasick;
use anyhow::{Context, Result, bail};
use regex::Regex;
use std::collections::HashSet;

use crate::config;
use crate::model::{Axis, Layer};
use crate::norm::Normalized;

pub struct Rule {
    pub axis: Axis,
    pub id: &'static str,
    pub weight: f64,
    pub terms: &'static [&'static str],
}

pub struct RegexRule {
    pub axis: Axis,
    pub id: &'static str,
    pub weight: f64,
    pub pattern: &'static str,
}

/// Shortest term allowed after normalization. Shorter stems match across word
/// boundaries and drown the report; abbreviations go to `REGEXES`, where word
/// boundaries hold.
pub const MIN_TERM_LEN: usize = 4;

/// Terms are written as stems, without endings: the search runs as a substring
/// over the collapsed normalized text, so `войн` matches every inflection.
/// Latin terms are transliterated by the same normalization as the corpus, so
/// `bybit` in the dictionary meets `bybit` in the text; a Cyrillic spelling
/// («байбит») is a separate term with a different normal form.
#[rustfmt::skip]
pub const RULES: &[Rule] = &[
    Rule {
        axis: Axis::Crypto,
        id: "exchanges",
        weight: 2.5,
        terms: &[
            "bybit", "байбит", "binance", "бинанс", "okx", "kucoin",
            "bitget", "mexc", "huobi", "coinbase", "kraken", "gateio",
            "garantex", "обменник", "биржа крипт",
        ],
    },
    Rule {
        axis: Axis::Crypto,
        id: "wallets",
        weight: 2.5,
        terms: &[
            "metamask", "trust wallet", "cake wallet",
            "tonkeeper", "тонкипер",
            "phantom", "ledger", "trezor", "холодный кошел",
            "seed phrase", "сид фраз", "мнемоник", "приватный ключ",
        ],
    },
    Rule {
        axis: Axis::Crypto,
        id: "assets_ops",
        weight: 2.0,
        terms: &[
            "крипт", "биткоин", "bitcoin", "эфириум", "ethereum",
            "usdt", "юсдт", "tether", "тезер", "стейбл", "альткоин",
            "вывел на карт", "закинул на бирж",
            "фиат", "стейкинг", "деривы", "фьючерс", "ликвидац",
            "кошел",
        ],
    },
    Rule {
        axis: Axis::Emigration,
        id: "intent",
        weight: 2.0,
        terms: &[
            "релокац", "релокейт", "эмигрир", "иммигрир",
            "вид на жительств",
            "уехать навсегд", "не вернус",
            "валить из",
            "digital nomad", "цифровой кочевник",
            "второй паспорт", "получить гражданств",
            "нострифик", "релокант",
        ],
    },
    // Resort countries are deliberately absent: they belong to the whitelist
    // in config.rs. Here only the directions a border officer reads as
    // relocation. The tier decides more than the term does: a relative who
    // lived in Serbia a year ago is ordinary life, three months ago is a risk.
    Rule {
        axis: Axis::Emigration,
        id: "hot_countries",
        weight: 1.5,
        terms: &[
            "тбилиси", "батуми", "вгрузи", "ереван", "армени",
            "серби", "белград", "черногор", "будва",
            "казахстан", "алматы", "астана", "бишкек",
            "аргентин", "буэнос", "израил",
        ],
    },
    Rule {
        axis: Axis::Emigration,
        id: "exit_logistics",
        weight: 1.5,
        terms: &[
            "продать квартир", "продал квартир",
            "вывести деньг", "закрыть счет",
            "билет в один конец",
        ],
    },
    // Kept under its own id so that a repeat run can pick up its windows with
    // `triage --since ... --new-rule leaving_question`.
    Rule {
        axis: Axis::Emigration,
        id: "leaving_question",
        weight: 1.5,
        terms: &["уедеш", "уезжаеш"],
    },
    Rule {
        axis: Axis::ForeignFinance,
        id: "kz_banks",
        weight: 2.0,
        terms: &[
            // `каспи` dropped: it caught the Caspian Sea. Latin `kaspi` is safe.
            "kaspi", "халык", "фридом банк", "freedom bank",
            "жусан", "форте банк", "береке",
            "центркредит", "centercredit", "алтын банк", "нурбанк",
        ],
    },
    Rule {
        axis: Axis::ForeignFinance,
        id: "payment_rails",
        weight: 1.5,
        terms: &[
            "payoneer", "пейонир", "revolut", "револют", "paysera", "пейсера",
            "monobank", "wise transfer", "transferwise",
            // `bunq` dropped: four letters, but a substring of the ordinary
            // word for a bunker.
        ],
    },
    Rule {
        axis: Axis::ForeignFinance,
        id: "account_ids",
        weight: 2.0,
        terms: &[
            "индивидуальный идентификационный номер", "казахстанский иин",
            "номер счета в банке", "реквизиты счета",
            "нерезидентск", "счет нерезидента",
            // `мультивалютн` dropped: it caught ads for a multi-currency
            // CRYPTO wallet, which is the crypto axis, not a foreign bank.
        ],
    },
    // Wording that shows OWNERSHIP of a foreign account or card rather than
    // an abstract conversation about banks. This is what the axis is for.
    Rule {
        axis: Axis::ForeignFinance,
        id: "ownership_talk",
        weight: 2.5,
        terms: &[
            "зарубежный счет", "заграничный счет", "иностранный счет",
            "счет за границей", "счет в казахстане", "счет в армении",
            "счет в грузии", "счет в сербии", "открыл счет", "открыть счет",
            "казахская карта", "казахстанская карта", "иностранная карта",
            "зарубежная карта", "заграничная карта", "нероссийская карта",
            "карта казахстана", "карта нерезидента", "виртуальная карта",
            "оформил карту", "выпустил карту", "завел карту",
        ],
    },
    Rule {
        axis: Axis::ForeignFinance,
        id: "proxy_payments",
        weight: 1.5,
        terms: &[
            "оплатить с моей карты", "оплачу своей картой", "скинь я оплачу",
            "оплати с казахской", "могу оплатить подписку", "оплатил подписку",
            "с моей карты оплат", "давай через мою карту",
        ],
    },
    // Links count too: hrefs from message entities are folded into the text
    // at ingest, so a domain fires the same way a name does. One outlet is
    // named after the word for rain: only its full name and domain are listed.
    Rule {
        axis: Axis::ForeignMedia,
        id: "outlets",
        weight: 2.0,
        terms: &[
            "медуз", "meduza", "медиазон", "mediazona",
            "новая газет",
            "радио свобод", "svoboda.org",
            "телеканал дожд", "tvrain",
            "the insider", "важные истор",
            "istories", "верстк", "вёрстк", "холод медиа",
            "настоящее врем", "эхо москв",
            "бибиси", "русская служба би-би-си",
        ],
    },
    Rule {
        axis: Axis::Lgbt,
        id: "terms",
        weight: 1.5,
        terms: &[
            "лгбт", "квир", "гомосексуал", "трансгендер", "небинарн",
            "каминг-аут", "однополый брак",
            "радужн",
        ],
    },
    Rule {
        axis: Axis::Politics,
        id: "war",
        weight: 2.0,
        terms: &[
            "войн", "спецоперац", "мобилизац", "вторжен", "оккупац",
            "военкомат", "повестк", "дезертир", "уклонист", "срочник",
            "обстрел", "бомбёж", "бомбеж", "ракетн", "шахед", "хаймарс",
            "мясорубк", "окоп", "чвк вагнер",
        ],
    },
    Rule {
        axis: Axis::Politics,
        id: "ukraine",
        weight: 2.0,
        terms: &[
            "украин", "зеленск", "киев", "харьков", "мариупол",
            "херсон", "донбасс", "донецк", "луганск", "азовсталь",
            "бахмут", "авдеевк", "запорожск", "одесс",
        ],
    },
    Rule {
        axis: Axis::Politics,
        id: "opposition_persons",
        weight: 2.5,
        terms: &[
            "навальн", "ходорковск", "каспаров", "яшин", "карамурза",
            "певчих", "жданов", "гудков", "милов", "чичваркин",
        ],
    },
    Rule {
        axis: Axis::Politics,
        id: "banned_orgs",
        weight: 2.5,
        terms: &[
            "умное голосован", "мемориал", "овдинфо",
            "либертарианск", "открытое пространств",
            "иноагент", "нежелательная организац", "экстремистск",
        ],
    },
    Rule {
        axis: Axis::Politics,
        id: "regime_talk",
        weight: 1.5,
        terms: &[
            "путин", "кремл", "росгвард", "силовик", "репресс",
            "политзаключ", "дискредитац", "цензур", "пропаганд",
            "диктатур", "автократ",
        ],
    },
];

/// Matched against the raw text: machine artifacts must not be normalized,
/// and abbreviations need word boundaries the collapsed form cannot give.
#[rustfmt::skip]
pub const REGEXES: &[RegexRule] = &[
    RegexRule {
        axis: Axis::Crypto,
        id: "acronyms_crypto",
        weight: 2.0,
        pattern: r"(?i)\b(btc|eth|ton|usdt|p2p|nft|defi|dex|cex)\b",
    },
    // crypto_hard is not lifted by the whitelist: a seed phrase stays a seed
    // phrase even inside holiday chatter.
    RegexRule {
        axis: Axis::CryptoHard,
        id: "btc_address",
        weight: 3.0,
        pattern: r"\b(bc1[ac-hj-np-z02-9]{11,71}|[13][a-km-zA-HJ-NP-Z1-9]{25,34})\b",
    },
    RegexRule {
        axis: Axis::CryptoHard,
        id: "eth_address",
        weight: 3.0,
        pattern: r"\b0x[a-fA-F0-9]{40}\b",
    },
    RegexRule {
        axis: Axis::CryptoHard,
        id: "ton_address",
        weight: 3.0,
        pattern: r"\b[EU]Q[A-Za-z0-9_-]{46}\b",
    },
    RegexRule {
        axis: Axis::CryptoHard,
        id: "tron_address",
        weight: 3.0,
        pattern: r"\bT[1-9A-HJ-NP-Za-km-z]{33}\b",
    },
    RegexRule {
        axis: Axis::CryptoHard,
        id: "tx_hash",
        weight: 2.0,
        pattern: r"\b[a-fA-F0-9]{64}\b",
    },
    // 12 or 24 lowercase Latin words in a row. Knowingly noisy — ordinary
    // English prose of the same shape matches — but a false hit costs one LLM
    // call and a missed seed phrase costs everything.
    RegexRule {
        axis: Axis::CryptoHard,
        id: "bip39_like",
        weight: 3.0,
        pattern: r"\b([a-z]{3,8} ){11}[a-z]{3,8}\b",
    },
    RegexRule {
        axis: Axis::Emigration,
        id: "acronyms_migration",
        weight: 2.0,
        pattern: r"(?i)\b(внж|пмж|гринкард|green ?card)\b",
    },
    // Card numbers are caught by shape, not by a list: a list of numbers in a
    // repository is still a list of card numbers. BIN Visa (4), Mastercard
    // (51-55, 22-27), any separators.
    RegexRule {
        axis: Axis::ForeignFinance,
        id: "card_number",
        weight: 3.0,
        pattern: r"\b(?:4\d{3}|5[1-5]\d{2}|2[2-7]\d{2})[ \-]?\d{4}[ \-]?\d{4}[ \-]?\d{4}\b",
    },
    RegexRule {
        axis: Axis::ForeignFinance,
        id: "kz_iin",
        weight: 2.0,
        pattern: r"(?i)\b(?:иин|iin)\b[^\d]{0,12}\d{12}\b",
    },
    RegexRule {
        axis: Axis::ForeignFinance,
        id: "iban",
        weight: 3.0,
        pattern: r"\b(?:KZ|GE|AM|RS|TR|LT|EE|LV|PL|DE|NL|ES|PT|CY)\d{2}[A-Z0-9]{11,28}\b",
    },
    // SWIFT/BIC needs a nearby keyword, or the shape catches any eight-letter
    // acronym in caps.
    RegexRule {
        axis: Axis::ForeignFinance,
        id: "swift_bic",
        weight: 2.0,
        pattern: r"(?i)\b(?:swift|бик|bic)\b[^A-Za-z0-9]{0,10}[A-Z]{4}[A-Z]{2}[A-Z0-9]{2}\b",
    },
    RegexRule {
        axis: Axis::ForeignMedia,
        id: "media_acronyms",
        weight: 1.5,
        pattern: r"(?i)\b(bbc|dw|rfe/rl|радио ?свобода)\b",
    },
    RegexRule {
        axis: Axis::Lgbt,
        id: "lgbt_acronyms",
        weight: 1.5,
        pattern: r"(?i)\b(лгбт\+?|lgbt\+?|lgbtq\+?)\b",
    },
    // Word boundaries keep a three-letter acronym out of longer words.
    RegexRule {
        axis: Axis::Politics,
        id: "acronyms_war",
        weight: 2.0,
        pattern: r"(?i)\b(сво|всу|тцк|днр|лнр|ципсо|чвк)\b",
    },
    RegexRule {
        axis: Axis::Politics,
        id: "acronyms_orgs",
        weight: 2.5,
        pattern: r"(?i)\b(фбк|фсб|мвд|ск ?рф)\b",
    },
];

struct Term {
    axis: Axis,
    rule_id: &'static str,
    term: &'static str,
    weight: f64,
    /// A multi-word term («вид на жительство») may cross spaces; a single-word
    /// one may not.
    multiword: bool,
}

#[derive(Debug, Clone)]
pub struct Match {
    pub axis: Axis,
    pub rule_id: String,
    pub term: String,
    pub weight: f64,
    pub layer: Layer,
    pub surface: Option<String>,
}

pub struct Dictionary {
    ac: AhoCorasick,
    terms: Vec<Term>,
    regexes: Vec<(Axis, &'static str, f64, Regex)>,
    protect: AhoCorasick,
    unprotect: AhoCorasick,
}

impl Dictionary {
    pub fn new() -> Result<Self> {
        let mut terms = Vec::new();
        let mut pats = Vec::new();
        let mut short = Vec::new();
        // Different spellings often share one normal form. A duplicate would
        // double the weight, so the first one wins and the rest are dropped.
        let mut seen = HashSet::new();

        for rule in RULES {
            for term in rule.terms {
                let norm = Normalized::build(term).text;
                if norm.chars().count() < MIN_TERM_LEN {
                    short.push(format!(
                        "{}:{} `{term}` → `{norm}`",
                        rule.axis.as_str(),
                        rule.id
                    ));
                    continue;
                }
                if !seen.insert(norm.clone()) {
                    continue;
                }
                pats.push(norm);
                terms.push(Term {
                    axis: rule.axis,
                    rule_id: rule.id,
                    term,
                    weight: rule.weight,
                    multiword: term.split_whitespace().count() > 1,
                });
            }
        }

        if !short.is_empty() {
            bail!(
                "terms shorter than {MIN_TERM_LEN} chars once normalized — they match across word boundaries:\n  {}",
                short.join("\n  ")
            );
        }

        let mut regexes = Vec::with_capacity(REGEXES.len());
        for r in REGEXES {
            let re = Regex::new(r.pattern).with_context(|| format!("bad regex `{}`", r.id))?;
            regexes.push((r.axis, r.id, r.weight, re));
        }

        let normalize = |v: &[&str]| -> Vec<String> {
            v.iter()
                .map(|t| Normalized::build(t).text)
                .filter(|s| !s.is_empty())
                .collect()
        };

        Ok(Self {
            ac: AhoCorasick::new(&pats)?,
            terms,
            regexes,
            protect: AhoCorasick::new(normalize(config::WHITELIST))?,
            unprotect: AhoCorasick::new(normalize(config::WHITELIST_OVERRIDE))?,
        })
    }

    pub fn term_count(&self) -> usize {
        self.terms.len()
    }

    pub fn regex_count(&self) -> usize {
        self.regexes.len()
    }

    /// Matches on one message, deduplicated by (rule_id, term): a repeated
    /// term must not multiply the weight.
    pub fn scan(&self, raw: &str, n: &Normalized) -> Vec<Match> {
        let mut seen: HashSet<(&str, &str)> = HashSet::new();
        let mut out = Vec::new();

        for m in self.ac.find_overlapping_iter(&n.text) {
            let start = n.text[..m.start()].chars().count();
            if !n.at_word_start(start) {
                continue;
            }
            let end = n.text[..m.end()].chars().count();
            let t = &self.terms[m.pattern().as_usize()];
            // A four-letter stem matched across a space 264 times before this.
            if !t.multiword && !n.single_word_ok(start, end) {
                continue;
            }
            if !seen.insert((t.rule_id, t.term)) {
                continue;
            }
            out.push(Match {
                axis: t.axis,
                rule_id: t.rule_id.to_string(),
                term: t.term.to_string(),
                weight: t.weight,
                layer: Layer::Keyword,
                surface: n.surface(start, end).map(|(a, b)| cut(raw, a, b)),
            });
        }

        for (axis, id, w, re) in &self.regexes {
            if let Some(m) = re.find(raw) {
                if !seen.insert((id, id)) {
                    continue;
                }
                out.push(Match {
                    axis: *axis,
                    rule_id: id.to_string(),
                    term: id.to_string(),
                    weight: *w,
                    layer: Layer::Regex,
                    surface: Some(clip(m.as_str())),
                });
            }
        }

        out
    }

    /// Protected: a whitelisted term is present and nothing overrides it.
    pub fn is_protected(&self, n: &Normalized) -> bool {
        self.protect.is_match(&n.text) && !self.unprotect.is_match(&n.text)
    }
}

fn cut(s: &str, a: usize, b: usize) -> String {
    clip(
        &s.chars()
            .skip(a)
            .take(b.saturating_sub(a))
            .collect::<String>(),
    )
}

fn clip(s: &str) -> String {
    if s.chars().count() <= 80 {
        s.to_string()
    } else {
        s.chars().take(80).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every term must be long enough and must not shadow another one: both
    /// are silent failures otherwise — noise in the report or a doubled weight.
    #[test]
    fn terms_are_usable_and_distinct() {
        let mut seen: Vec<(String, &str)> = Vec::new();
        for rule in RULES {
            for term in rule.terms {
                let norm = Normalized::build(term).text;
                assert!(
                    norm.chars().count() >= MIN_TERM_LEN,
                    "{}:{} `{term}` → `{norm}` is too short",
                    rule.axis.as_str(),
                    rule.id
                );
                if let Some((_, first)) = seen.iter().find(|(n, _)| *n == norm) {
                    panic!(
                        "{}:{} `{term}` has the same normal form as `{first}`",
                        rule.axis.as_str(),
                        rule.id
                    );
                }
                seen.push((norm, term));
            }
        }
    }

    #[test]
    fn matches_by_stem_and_across_scripts() {
        let d = Dictionary::new().unwrap();
        let hits = |s: &str| -> Vec<String> {
            d.scan(s, &Normalized::build(s))
                .into_iter()
                .map(|m| format!("{}:{}", m.axis.as_str(), m.rule_id))
                .collect()
        };
        assert!(hits("закупился на bybit").contains(&"crypto:exchanges".into()));
        assert!(hits("сидели в окопах").contains(&"politics:war".into()));
        // The same word inside another one must not fire.
        assert!(hits("быстро копится работа").is_empty());
    }
}
