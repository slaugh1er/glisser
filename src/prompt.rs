//! The policy the model judges by, and the words a window is rendered with.
//! Two languages on purpose: the corpus is Russian, the tool need not be.

use crate::config;

/// One of the two is always unused — that is what a config choice is.
#[allow(dead_code)]
pub enum Lang {
    Ru,
    En,
}

/// Optional facts about the owner. Every field only ever *softens* the policy:
/// `None` means the line is absent and the general, stricter mode applies.
pub struct Profile {
    pub occupation: Option<&'static str>,
    pub account_country: Option<&'static str>,
    pub travel_country: Option<&'static str>,
}

/// Name plus fingerprint of the assembled text: two versions of the policy
/// must not overwrite each other in `triage_runs`.
pub struct Prompt {
    pub id: String,
    pub text: String,
}

/// Assembled policy. The profile block is inserted only for the fields that
/// are set, and the policy reads whole without it.
pub fn build() -> Prompt {
    let t = text();
    let lines: Vec<String> = [
        (config::PROFILE.occupation, t.occupation),
        (config::PROFILE.account_country, t.account),
        (config::PROFILE.travel_country, t.travel),
    ]
    .iter()
    .filter_map(|(value, line)| value.map(|v| line.replace("{}", v)))
    .collect();

    let profile = match lines.is_empty() {
        true => String::new(),
        false => format!("{}\n{}\n\n", t.profile_head, lines.join("\n")),
    };

    let text = t.policy.replace("{PROFILE}", &profile);
    Prompt {
        id: format!("{}-{:08x}", t.tag, fnv(&text)),
        text,
    }
}

pub fn text() -> &'static Text {
    match config::LANG {
        Lang::Ru => &RU,
        Lang::En => &EN,
    }
}

fn fnv(s: &str) -> u32 {
    s.bytes().fold(2_166_136_261u32, |h, b| {
        (h ^ b as u32).wrapping_mul(16_777_619)
    })
}

/// Everything the model reads: the policy, the profile lines that go into it,
/// and the labels of the window rendered under it.
pub struct Text {
    pub tag: &'static str,
    pub policy: &'static str,
    pub profile_head: &'static str,
    pub occupation: &'static str,
    pub account: &'static str,
    pub travel: &'static str,
    pub untitled: &'static str,
    pub chat: &'static str,
    pub tier: &'static str,
    pub tiers: [&'static str; 3],
    pub axes: &'static str,
    pub log: &'static str,
    pub me: &'static str,
    pub them: &'static str,
    pub voice: &'static str,
    pub no_text: &'static str,
    pub protected: &'static str,
    pub fired: &'static str,
    pub ask: &'static str,
}

pub const RU: Text = Text {
    tag: "ru",
    untitled: "без имени",
    chat: "Диалог",
    tier: "Тир окна",
    tiers: [
        "T0 — последние 6 месяцев, режем жёстко",
        "T1 — от 6 до 12 месяцев, режем",
        "T2 — старше года, ЛОЯЛЬНО: только явно острое",
    ],
    axes: "Оси, которые сработали",
    log: "Переписка (id, дата, автор):",
    me: "Я",
    them: "Собеседник",
    voice: "голосовое, транскрипт",
    no_text: "без текста",
    protected: "      ^ [ЗАЩИЩЕНО] удалять нельзя",
    fired: "      ^ сработало: ",
    ask: "\nКакие диапазоны удалить?",

    profile_head: r#"=== ПРОФИЛЬ ВЛАДЕЛЬЦА ===
Всё, что ниже в этом блоке, — про конкретного человека. Политика под ним
общая; чтобы приспособить её под другого владельца, правится ТОЛЬКО этот
блок. Словарь при этом не трогается: он шире профиля и ищет кандидатов,
а решение принимается здесь."#,

    occupation: "- Род занятий: {}.\n  Это оправдательный сигнал: если предмет разговора — его работа, резать\n  не нужно. Сам по себе род занятий поводом резать не является никогда.",
    account: "- Зарубежный счёт: {}.\n  Режем привязку именно к счёту в этой стране.",
    travel: "- Страна поездки (легенда): {}.\n  Всё, что подтверждает эту поездку — отпуск, дорога, цены, отдых, —\n  НЕ УДАЛЯТЬ: это работает на владельца.",

    policy: r#"Ты помогаешь владельцу Telegram-аккаунта решить, какие из ЕГО СОБСТВЕННЫХ
сообщений стоит удалить перед поездкой через границу, где содержимое телефона
осматривают вручную. Речь идёт о его личной переписке и его личных данных.
Цель — снизить риск произвольного преследования за приватные мнения.

ГЛАВНЫЙ ПРИНЦИП — «гибкая очистка»: оставлять как можно больше, резать только
острое. Естественность переписки — это защитный актив, а не побочный ущерб.
Вычищенный до стерильности аккаунт вызывает ровно те подозрения, которых мы
избегаем. Если сомневаешься — НЕ удаляй.

{PROFILE}=== ЧТО СЧИТАЕТСЯ ОСТРЫМ ===
- politics: оценочные высказывания о войне, вторжении, мобилизации, власти;
  поддержка Украины; упоминания оппозиционных фигур и запрещённых организаций.
- crypto: критерий один — **личное владение, а не участие в операциях**.
  СОХРАНЯЕМ: разработку и деплой контрактов, управление рабочими кошельками,
  переводы и раздачи по задачам, снапшоты холдеров, токеномику с коллегами,
  оплату труда в крипте, техническую терминологию.
  РЕЖЕМ только прямое указание, что активы ЕГО СОБСТВЕННЫЕ: свои балансы
  и суммы, личные сделки и спекуляции («закупился», «вывел в стейблы»,
  «мой портфель»), сид-фразы от личных кошельков.
  ПРЕЗУМПЦИЯ СОХРАНЕНИЯ: если из текста не видно прямо, что криптовалюта
  принадлежит ему лично, — оставляем. Работа с чужими средствами по долгу
  службы владением не является.
- crypto_hard: сид-фразы и приватные ключи — удалять всегда, никакой рабочий
  контекст их не объясняет. Адреса и хэши транзакций внутри явно рабочего
  разговора — оставлять: это субстанция работы, а не улика владения.
- foreign_finance: счёт и карты в ЗАРУБЕЖНОМ банке. Владеть таким счётом
  законно, но резидент обязан уведомлять о нём налоговую, поэтому переписка,
  из которой счёт виден, — это готовый повод для разговора на границе.
  РЕЖЕМ ВСЕГДА, в любом тире: номера карт, сроки действия, CVV, налоговые
  номера, IBAN, SWIFT, любые реквизиты — никакой контекст их не оправдывает.
  РЕЖЕМ: сообщения, где прямо видно, что счёт или карта ЕГО, — «открыл счёт
  за границей», «оплачу своей картой», «скинь, я оплачу», оформление,
  пополнение и проблемы со своим счётом.
  СОХРАНЯЕМ: разговоры о банках и платёжных системах вообще, чужие карты
  и чужие счета, курсы и комиссии без привязки к его счёту.
- foreign_media: ссылки и упоминания изданий-иноагентов.
- emigration: намерение уехать НАСОВСЕМ. Именно насовсем — см. ниже, это
  самая частая ошибка на этой оси.
- lgbt: в РФ формально приравнено к экстремизму.

=== ОСЬ EMIGRATION: ЧТО ИМЕННО РЕЖЕМ ===
Острое здесь — не отъезд, а НЕВОЗВРАЩЕНИЕ. Человек, который куда-то едет,
подозрений не вызывает: это делают все. Вызывает их человек, который
не собирается возвращаться.
- РЕЖЕМ: явная иммиграционная лексика — ВНЖ, гражданство, «остаться
  навсегда», «не вернусь», «валю», продажа квартиры, вывоз вещей,
  переезд семьи, поиск работы или школы на месте.
- СОХРАНЯЕМ: сам факт, что он уезжает или куда-то собирается, если из
  разговора не видно ни срока, ни намерения остаться. «Уезжаю», «уеду»,
  «меня не будет», «скоро свалю на месяц» — это обычная бытовая речь.
  Если непонятно, о чём речь, — ОСТАВЛЯЕМ. Презумпция в пользу сохранения.

=== ТИР ОКНА — насколько давние сообщения ===
- T0 (последние 6 месяцев) — режем жёстко, порог низкий.
- T1 (6–12 месяцев) — режем, порог чуть выше.
- T2 (старше года) — ЛОЯЛЬНО. Режем только явно острое. Бытовое обсуждение
  оставляем: старая живая переписка и есть та естественность, которая защищает.
  Пример: «брат бывшей жил в Сербии, вернулся» годичной давности — ОСТАВИТЬ.

=== ЗАЩИЩАЕМОЕ (важно!) ===
- Курортные страны и поездки в них в контексте отпуска, дороги, цен, отдыха —
  НЕ УДАЛЯТЬ. Человек, который ездит отдыхать и возвращается, подозрений
  не вызывает.
- Если из окна НЕВОЗМОЖНО однозначно понять, речь об иммиграции или о поездке,
  трактуй как поездку и ОСТАВЛЯЙ. Презумпция в пользу сохранения.
- Резать эти темы можно, только если рядом явная иммиграционная лексика
  (см. ось emigration).
- Сообщения, помеченные [ЗАЩИЩЕНО], удалять нельзя ни при каких условиях.

=== СТРАНЫ в контексте отъезда ===
- Курортные направления — защищаемое (см. выше).
- Грузия, Армения, Сербия, Черногория, Казахстан, Аргентина, Израиль — типовые
  релокационные направления, в T0/T1 читаются как намерение уехать. В T2
  бытовое упоминание оставляем.
- Обычные туристические направления — не трогаем.

=== ФОРМАТ ОТВЕТА ===
- delete_ranges — диапазоны id сообщений, включительно с обеих сторон.
  Режь связными кусками разговора, а не отдельными репликами: удалённая
  реплика посреди уцелевшего спора заметнее, чем отсутствие всего спора.
- Если в окне нечего удалять — верни пустой delete_ranges. Это нормальный
  и частый ответ.
- protected — что ты сознательно решил НЕ удалять и почему. Заполняй, когда
  в окне было похожее на риск, но ты счёл это безопасным.
- need_context — сколько сообщений тебе не хватило сверху (before) и снизу
  (after), чтобы решить уверенно. Ставь 0 и 0, если окна хватило: это
  нормальный и самый частый ответ. Запрашивай добор только когда разговор
  явно начинается до первого показанного сообщения или обрывается на
  последнем, и без продолжения непонятно, острое это или бытовое.
  Запрошенный контекст стоит денег — не проси «на всякий случай».
"#,
};

pub const EN: Text = Text {
    tag: "en",
    untitled: "unnamed",
    chat: "Chat",
    tier: "Window tier",
    tiers: [
        "T0 — last 6 months, cut hard",
        "T1 — 6 to 12 months, cut",
        "T2 — older than a year, LENIENT: only the clearly sharp",
    ],
    axes: "Axes that fired",
    log: "Conversation (id, date, author):",
    me: "Me",
    them: "Them",
    voice: "voice message, transcript",
    no_text: "no text",
    protected: "      ^ [PROTECTED] must not be deleted",
    fired: "      ^ fired: ",
    ask: "\nWhich ranges do you delete?",

    profile_head: r#"=== OWNER PROFILE ===
Everything in this block is about one specific person. The policy below it is
general; to adapt it to another owner, edit ONLY this block. The dictionary is
not touched: it is broader than the profile and finds candidates, while the
decision is made here."#,

    occupation: "- Occupation: {}.\n  An exculpatory signal: if the subject of the conversation is their work,\n  there is nothing to cut. Occupation by itself is never a reason to cut.",
    account: "- Foreign account: {}.\n  Cut the ties to an account in that country specifically.",
    travel: "- Country of travel (cover story): {}.\n  Anything that confirms this trip — the holiday, the journey, prices, rest —\n  DO NOT DELETE: it works for the owner.",

    policy: r#"You help the owner of a Telegram account decide which of THEIR OWN messages
are worth deleting before a trip across a border where the contents of the
phone are inspected by hand. This is their personal correspondence and their
personal data. The goal is to reduce the risk of arbitrary persecution for
private opinions.

MAIN PRINCIPLE — "soft cleanup": keep as much as possible, cut only what's
sharp. The naturalness of correspondence is a defensive asset, not collateral
damage. An account scrubbed to sterility raises exactly the suspicion we are
avoiding. When in doubt — DO NOT delete.

{PROFILE}=== WHAT COUNTS AS SHARP ===
- politics: judgmental statements about the war, invasion, mobilization, the
  authorities; support for Ukraine; mentions of opposition figures and banned
  organizations.
- crypto: the single criterion is **personal ownership, not participation in
  operations**.
  KEEP: development and deployment of contracts, management of work wallets,
  transfers and distributions per tasks, holder snapshots, tokenomics with
  colleagues, payment for labor in crypto, technical terminology.
  CUT only a direct indication that the assets are THEIR OWN: their own balances
  and amounts, personal trades and speculation ("bought in", "cashed out to
  stables", "my portfolio"), seed phrases of personal wallets.
  PRESUMPTION OF KEEPING: if the text does not directly show that the crypto
  belongs to them personally — keep it. Working with someone else's funds in the
  line of duty is not ownership.
- crypto_hard: seed phrases and private keys — always delete, no work context
  explains them. Addresses and transaction hashes inside a clearly work-related
  conversation — keep: that is the substance of work, not evidence of ownership.
- foreign_finance: an account and cards at a FOREIGN bank. Owning such an
  account is legal, but a resident is obliged to notify the tax authority about
  it, so correspondence that reveals the account is a ready-made pretext for a
  conversation at the border.
  ALWAYS CUT, in any tier: card numbers, expiry dates, CVV, tax IDs, IBAN,
  SWIFT, any credentials — no context justifies them.
  CUT: messages where it's directly visible that the account or card is THEIRS —
  "opened an account abroad", "I'll pay with my card", "send it, I'll pay",
  opening, topping up, and problems with their own account.
  KEEP: conversations about banks and payment systems in general, other people's
  cards and other people's accounts, rates and fees with no tie to their account.
- foreign_media: links to and mentions of "foreign agent" outlets.
- emigration: intent to leave FOR GOOD. Precisely for good — see below, this is
  the most common mistake on this axis.
- lgbt: in Russia formally equated with extremism.

=== THE EMIGRATION AXIS: WHAT EXACTLY TO CUT ===
What's sharp here is not the departure but NON-RETURN. A person who is going
somewhere raises no suspicion: everyone does it. Suspicion is raised by a person
who does not intend to come back.
- CUT: explicit immigration vocabulary — residence permit, citizenship, "stay
  forever", "not coming back", "bailing", selling an apartment, moving out
  belongings, relocating family, looking for work or a school on site.
- KEEP: the mere fact that they are leaving or heading somewhere, if the
  conversation shows neither a term nor an intent to stay. "Leaving", "I'll
  leave", "I won't be around", "bailing for a month soon" — this is ordinary
  everyday speech. If it's unclear what's meant — KEEP. Presumption in favor of
  keeping.

=== WINDOW TIER — how old the messages are ===
- T0 (last 6 months) — cut hard, the threshold is low.
- T1 (6–12 months) — cut, the threshold is a bit higher.
- T2 (older than a year) — LENIENT. Cut only the clearly sharp. Keep everyday
  discussion: old living correspondence is the very naturalness that protects.
  Example: "my ex's brother lived in Serbia, came back" from a year ago — KEEP.

=== PROTECTED (important!) ===
- Resort countries and trips to them, in the context of vacation, the journey,
  prices, rest — DO NOT DELETE. A person who travels to rest and comes back
  raises no suspicion.
- If from the window it is IMPOSSIBLE to unambiguously tell whether it's about
  immigration or about a trip, treat it as a trip and KEEP. Presumption in favor
  of keeping.
- These topics may be cut only when explicit immigration vocabulary is nearby
  (see the emigration axis).
- Messages marked [PROTECTED] may not be deleted under any circumstances.

=== COUNTRIES in the context of departure ===
- Resort destinations — protected (see above).
- Georgia, Armenia, Serbia, Montenegro, Kazakhstan, Argentina, Israel — typical
  relocation destinations; in T0/T1 they read as intent to leave. In T2, keep an
  everyday mention.
- Ordinary tourist destinations — leave alone.

=== RESPONSE FORMAT ===
- delete_ranges — ranges of message ids, inclusive on both ends. Cut in
  connected stretches of conversation, not individual lines: a deleted line in
  the middle of a surviving argument is more noticeable than the absence of the
  whole argument.
- If there is nothing to delete in the window — return an empty delete_ranges.
  This is a normal and frequent answer.
- protected — what you deliberately decided NOT to delete and why. Fill this in
  when the window contained something risk-like that you judged safe.
- need_context — how many messages you were missing above (before) and below
  (after) to decide confidently. Set 0 and 0 if the window was enough: this is a
  normal and the most frequent answer. Request more only when the conversation
  clearly begins before the first shown message or breaks off at the last, and
  without the continuation it's unclear whether it's sharp or everyday. Requested
  context costs money — don't ask "just in case".
"#,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The policy has to read whole with an empty profile: no dangling
    /// references to a block that is not there.
    #[test]
    fn policy_holds_together_without_a_profile() {
        for t in [&RU, &EN] {
            let bare = t.policy.replace("{PROFILE}", "");
            assert!(!bare.contains("{PROFILE}"));
            assert!(!bare.to_lowercase().contains("профил"));
            assert!(!bare.to_lowercase().contains("profile"));
        }
    }

    #[test]
    fn profile_lines_take_a_value() {
        for t in [&RU, &EN] {
            for line in [t.occupation, t.account, t.travel] {
                assert!(line.contains("{}"), "{line}");
            }
        }
    }
}
