//! What a Claude agent's own transcript says about where it stopped: whether
//! its last message asks the person something, how full its context is, which
//! model answered, and a usage limit it ran into. The server reads the tail of
//! the session's JSONL transcript and hands the result to the TUI as reserved
//! agent tokens, so the frozen client-shell codec is unchanged.
//!
//! Also here: the Claude logins this machine has (`~/.claude` and every
//! `~/.claude-<id>` with credentials, run as `claude` / `claude-<id>`) and what
//! a status line recorded about their rate limits.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// "1" when the agent's last message asks the person something.
pub(crate) const QUESTION_TOKEN: &str = "herdr.question";
/// The end of the agent's last message, for the sidebar's hover card.
pub(crate) const EXCERPT_TOKEN: &str = "herdr.excerpt";
/// Tokens in the agent's context window after its last reply.
pub(crate) const CONTEXT_TOKEN: &str = "herdr.context";
/// The model of the agent's last reply.
pub(crate) const MODEL_TOKEN: &str = "herdr.model";
/// The command the agent runs under: `claude` or `claude-<id>`.
pub(crate) const LOGIN_TOKEN: &str = "herdr.login";
/// JSON list of every login with what is known of its limits (`LoginStatus`).
pub(crate) const LOGINS_TOKEN: &str = "herdr.logins";
/// A usage limit stops the agent: when it resets in Unix seconds, or "login"
/// when the login itself expired.
pub(crate) const LIMIT_TOKEN: &str = "herdr.limit";
/// "1" while the server will send the continue prompt once the limit resets.
pub(crate) const CONTINUE_TOKEN: &str = "herdr.limit.continue";
/// JSON argv that resumes the agent's session under its login.
pub(crate) const RESUME_TOKEN: &str = "herdr.resume";

/// Every token above, which only the server may set.
pub(crate) const ALL_TOKENS: [&str; 9] = [
    QUESTION_TOKEN,
    EXCERPT_TOKEN,
    CONTEXT_TOKEN,
    MODEL_TOKEN,
    LOGIN_TOKEN,
    LOGINS_TOKEN,
    LIMIT_TOKEN,
    CONTINUE_TOKEN,
    RESUME_TOKEN,
];

/// How much of a transcript's end is read; a longer read follows when the last
/// reply sits further back (a huge tool result after it).
const TAIL_BYTES: u64 = 1 << 20;
const LONG_TAIL_BYTES: u64 = 8 << 20;
/// The hover card's excerpt, in characters.
const EXCERPT_CHARS: usize = 700;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TranscriptInsight {
    /// The final message of the turn the agent last ended, when nothing (a
    /// person's prompt, a tool call) came after it.
    pub(crate) last_message: Option<LastMessage>,
    /// Tokens in context after the last reply; none right after a compaction.
    pub(crate) context_tokens: Option<u64>,
    pub(crate) model: Option<String>,
    /// The newest event is a usage-limit error.
    pub(crate) limit: Option<LimitHit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LastMessage {
    /// The transcript entry's uuid, so a dismissed question stays dismissed.
    pub(crate) uuid: String,
    pub(crate) excerpt: String,
    pub(crate) asks: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LimitHit {
    /// A usage limit, lifted at this Unix time when the message said.
    Usage { resets_at: Option<u64> },
    /// The login itself expired: `/login` again or run under another login.
    Login,
}

impl LimitHit {
    pub(crate) fn token_value(self) -> String {
        match self {
            Self::Usage {
                resets_at: Some(at),
            } => at.to_string(),
            Self::Usage { resets_at: None } => "0".to_owned(),
            Self::Login => "login".to_owned(),
        }
    }

    pub(crate) fn from_token(value: &str) -> Option<Self> {
        match value {
            "login" => Some(Self::Login),
            "0" => Some(Self::Usage { resets_at: None }),
            value => value.parse().ok().map(|at| Self::Usage {
                resets_at: Some(at),
            }),
        }
    }

    /// Still holding the agent at `now`: a limit with a known reset is over once
    /// that time passes.
    pub(crate) fn holds_at(self, now: u64) -> bool {
        match self {
            Self::Usage {
                resets_at: Some(at),
            } => now < at,
            Self::Usage { resets_at: None } | Self::Login => true,
        }
    }
}

/// Reads what `path`'s transcript says. None when the file can't be read.
pub(crate) fn read_transcript_insight(
    path: &Path,
    utc_offset_secs: i64,
) -> Option<TranscriptInsight> {
    let (text, whole) = read_tail(path, TAIL_BYTES)?;
    let insight = parse_transcript_tail(&text, !whole, utc_offset_secs);
    if insight.model.is_some() || whole {
        return Some(insight);
    }
    let (text, whole) = read_tail(path, LONG_TAIL_BYTES)?;
    Some(parse_transcript_tail(&text, !whole, utc_offset_secs))
}

/// The last `max` bytes of a file, and whether that is all of it.
fn read_tail(path: &Path, max: u64) -> Option<(String, bool)> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.take(max).read_to_end(&mut bytes).ok()?;
    Some((String::from_utf8_lossy(&bytes).into_owned(), start == 0))
}

/// Reads transcript lines from the newest back. `cut` says the first line may
/// be the cut-off end of a longer one.
pub(crate) fn parse_transcript_tail(
    text: &str,
    cut: bool,
    utc_offset_secs: i64,
) -> TranscriptInsight {
    let mut lines = text.lines().collect::<Vec<_>>();
    if cut && !lines.is_empty() {
        lines.remove(0);
    }
    let mut insight = TranscriptInsight::default();
    // Whether the newest events after the last reply are settled: the limit by
    // the newest message of any kind, the last message by the newest reply.
    let mut limit_decided = false;
    let mut message_decided = false;
    let mut turn_open = false;
    let mut compacted = false;
    for line in lines.iter().rev() {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if entry
            .get("isSidechain")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            continue;
        }
        match entry.get("type").and_then(serde_json::Value::as_str) {
            Some("assistant") => {
                let message = entry.get("message");
                let text = message_text(message);
                if entry
                    .get("isApiErrorMessage")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                {
                    if !limit_decided {
                        limit_decided = true;
                        let at = entry
                            .get("timestamp")
                            .and_then(serde_json::Value::as_str)
                            .and_then(parse_iso_timestamp);
                        insight.limit = limit_from_text(&text, at, utc_offset_secs);
                    }
                    continue;
                }
                limit_decided = true;
                let model = message
                    .and_then(|message| message.get("model"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|model| !model.starts_with('<'));
                if insight.model.is_none() {
                    insight.model = model.map(str::to_owned);
                }
                if insight.context_tokens.is_none() && !compacted {
                    insight.context_tokens = message
                        .and_then(|message| message.get("usage"))
                        .map(context_tokens)
                        .filter(|tokens| *tokens > 0);
                }
                if !message_decided {
                    message_decided = true;
                    let calls_tool = message_has_block(message, "tool_use");
                    if !turn_open && !calls_tool && !text.trim().is_empty() {
                        insight.last_message = Some(LastMessage {
                            uuid: entry
                                .get("uuid")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            excerpt: excerpt(&text),
                            asks: asks_person(&text),
                        });
                    }
                }
                if insight.model.is_some() && (insight.context_tokens.is_some() || compacted) {
                    break;
                }
            }
            Some("user") => {
                if entry.get("isMeta").and_then(serde_json::Value::as_bool) == Some(true) {
                    continue;
                }
                limit_decided = true;
                if !message_decided {
                    turn_open = true;
                }
            }
            Some("system")
                if entry.get("subtype").and_then(serde_json::Value::as_str)
                    == Some("compact_boundary")
                    && insight.context_tokens.is_none() =>
            {
                compacted = true;
                if !message_decided {
                    turn_open = true;
                    message_decided = true;
                }
            }
            _ => {}
        }
    }
    insight
}

fn message_text(message: Option<&serde_json::Value>) -> String {
    let Some(content) = message.and_then(|message| message.get("content")) else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(serde_json::Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn message_has_block(message: Option<&serde_json::Value>, kind: &str) -> bool {
    message
        .and_then(|message| message.get("content"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(serde_json::Value::as_str) == Some(kind))
        })
}

fn context_tokens(usage: &serde_json::Value) -> u64 {
    [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
        "output_tokens",
    ]
    .into_iter()
    .filter_map(|key| usage.get(key).and_then(serde_json::Value::as_u64))
    .sum()
}

fn paragraphs(text: &str) -> Vec<&str> {
    let mut paragraphs = Vec::new();
    let mut start = None;
    let mut end = 0;
    for (offset, line) in line_offsets(text) {
        if line.trim().is_empty() {
            if let Some(from) = start.take() {
                paragraphs.push(&text[from..end]);
            }
        } else {
            start.get_or_insert(offset);
            end = offset + line.len();
        }
    }
    if let Some(from) = start {
        paragraphs.push(&text[from..end]);
    }
    paragraphs
}

fn line_offsets(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    text.split('\n').map(move |line| {
        let at = offset;
        offset += line.len() + 1;
        (at, line)
    })
}

/// Whether a message ends by asking the person: a question mark in its last
/// paragraph outside quotes, code and links, or, in its last three, a
/// recommendation between options or words that wait for an answer — unless
/// they say nothing waits. Checked against a week of real turns (22k turn
/// ends): it caught the message before 743 of 770 one-letter answers ("а",
/// "1 - б"), and of 60 flagged messages drawn at random about nine in ten
/// waited on the person; most of the rest were reports that still recalled an
/// earlier open question.
pub(crate) fn asks_person(text: &str) -> bool {
    static ASKS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)рекомендаци[яи]\b|рекомендую|жду (?:тво|ваш|от )|ждёт (?:тво|ваш)|(?:ваш|твой|твоего|вашего) (?:ответ|решени)|скажите|скажи[,:]|подтверди(?:те|шь)?\b|решени[ея] за (?:тобой|вами)|нужн[оы] (?:от тебя|от вас|ваш|тво)|нужно «|какой (?:вариант|берём|выбира)|что (?:делаем|выбираешь|выбираете)|\b(?:should i|do you want|would you like|let me know|which (?:option|one)|i recommend|waiting for your)\b",
        )
        .expect("question pattern")
    });
    // "Nothing waits for your decision now."
    static NOTHING_WAITS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)ничего не ждёт|не ждёт (?:тво|ваш)|ответ (?:ему |тебе |вам )?не нужен|(?:от тебя|от вас) (?:сейчас )?ничего не нужно|вопросов (?:к тебе |к вам )?нет",
        )
        .expect("nothing-waits pattern")
    });
    // "If you want another order, say so": an offer, not a question.
    static CONDITIONAL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)(?:если|когда)[^.!?\n]{0,80}(?:скажи|скажите|напиши)")
            .expect("conditional pattern")
    });
    // A question mark inside these is not the message's own.
    static QUOTED: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"«[^»]*»|"[^"]*"|“[^”]*”|`[^`]*`|https?://\S+"#)
            .expect("quoted pattern")
    });
    let paragraphs = paragraphs(text);
    let Some(last) = paragraphs.last() else {
        return false;
    };
    let tail = &paragraphs[paragraphs.len().saturating_sub(3)..];
    if tail
        .iter()
        .any(|paragraph| NOTHING_WAITS.is_match(paragraph))
    {
        return false;
    }
    if QUOTED.replace_all(last, "").contains('?') {
        return true;
    }
    tail.iter()
        .any(|paragraph| ASKS.is_match(&CONDITIONAL.replace_all(paragraph, "")))
}

/// The end of a message for the hover card: whole last paragraphs that fit,
/// or the tail of a longer last one. Markdown emphasis is dropped.
pub(crate) fn excerpt(text: &str) -> String {
    let clean = |paragraph: &str| paragraph.replace("**", "").replace('`', "");
    let mut taken = Vec::new();
    let mut length = 0;
    for paragraph in paragraphs(text).into_iter().rev() {
        let paragraph = clean(paragraph);
        let chars = paragraph.chars().count();
        if !taken.is_empty() && length + chars > EXCERPT_CHARS {
            break;
        }
        length += chars;
        taken.push(paragraph);
        if length >= EXCERPT_CHARS {
            break;
        }
    }
    taken.reverse();
    let joined = taken.join("\n\n").trim_end().to_owned();
    let chars = joined.chars().count();
    if chars <= EXCERPT_CHARS {
        return joined;
    }
    let tail = joined
        .chars()
        .skip(chars - EXCERPT_CHARS)
        .collect::<String>();
    format!("…{}", tail.trim_start())
}

/// Reads a limit message: "You've hit your 5-hour limit · resets 6:30pm
/// (Europe/Riga)", "… · resets Nov 20, 3pm", the older "usage limit
/// reached|<unix>", and "Login reached its time limit · Please run /login".
/// Other API errors (network, overload) are no limit.
pub(crate) fn limit_from_text(
    text: &str,
    at: Option<u64>,
    utc_offset_secs: i64,
) -> Option<LimitHit> {
    let lower = text.to_lowercase();
    if lower.contains("/login") || lower.contains("oauth token has expired") {
        return Some(LimitHit::Login);
    }
    if !lower.contains("limit") {
        return None;
    }
    if !(lower.contains("hit your") || lower.contains("limit reached") || lower.contains("resets"))
    {
        return None;
    }
    if let Some((_, rest)) = text.split_once('|') {
        if let Ok(at) = rest.trim().parse::<u64>() {
            return Some(LimitHit::Usage {
                resets_at: Some(at),
            });
        }
    }
    let resets_at = lower
        .find("resets")
        .and_then(|index| parse_reset_time(&lower[index + "resets".len()..], at?, utc_offset_secs));
    Some(LimitHit::Usage { resets_at })
}

/// "6:30pm", "12am", "18:30", optionally after a date: "nov 20, 3pm",
/// "nov 20 at 3pm". The next such local time after `at`.
fn parse_reset_time(text: &str, at: u64, utc_offset_secs: i64) -> Option<u64> {
    static TIME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"^\s*(?:(jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)[a-z]*\.?\s+(\d{1,2})(?:,|\s+at)?\s+)?(\d{1,2})(?::(\d{2}))?\s*(am|pm)?",
        )
        .expect("reset time pattern")
    });
    let captures = TIME.captures(text)?;
    let mut hour = captures.get(3)?.as_str().parse::<i64>().ok()?;
    let minute = captures
        .get(4)
        .map_or(Some(0), |minute| minute.as_str().parse::<i64>().ok())?;
    match captures.get(5).map(|meridiem| meridiem.as_str()) {
        Some("am") if hour == 12 => hour = 0,
        Some("pm") if hour < 12 => hour += 12,
        _ => {}
    }
    if hour > 23 || minute > 59 {
        return None;
    }
    let local_at = at as i64 + utc_offset_secs;
    let clock = hour * 3600 + minute * 60;
    let local = match (captures.get(1), captures.get(2)) {
        (Some(month), Some(day)) => {
            let month = [
                "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
            ]
            .iter()
            .position(|name| *name == month.as_str())? as u8
                + 1;
            let day = day.as_str().parse::<u8>().ok()?;
            let today = time::OffsetDateTime::from_unix_timestamp(local_at).ok()?;
            let mut year = today.year();
            loop {
                let date =
                    time::Date::from_calendar_date(year, time::Month::try_from(month).ok()?, day)
                        .ok()?;
                let local = date.midnight().assume_utc().unix_timestamp() + clock;
                if local > local_at || year > today.year() {
                    break local;
                }
                year += 1;
            }
        }
        _ => {
            let day_start = local_at - local_at.rem_euclid(86_400);
            let mut local = day_start + clock;
            if local <= local_at {
                local += 86_400;
            }
            local
        }
    };
    u64::try_from(local - utc_offset_secs).ok()
}

/// "2026-11-17T10:43:57.064Z" as Unix seconds.
pub(crate) fn parse_iso_timestamp(value: &str) -> Option<u64> {
    let value = value.strip_suffix('Z')?;
    let (date, clock) = value.split_once('T')?;
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let clock = clock.split('.').next()?;
    let mut clock = clock.split(':').map(str::parse::<i64>);
    let (hour, minute, second) = (
        clock.next()?.ok()?,
        clock.next()?.ok()?,
        clock.next()?.ok()?,
    );
    let date = time::Date::from_calendar_date(
        i32::try_from(year).ok()?,
        time::Month::try_from(u8::try_from(month).ok()?).ok()?,
        u8::try_from(day).ok()?,
    )
    .ok()?;
    let unix = date.midnight().assume_utc().unix_timestamp() + hour * 3600 + minute * 60 + second;
    u64::try_from(unix).ok()
}

/// A Claude session's transcript under any login's projects directory (all
/// logins usually share `~/.claude/projects`).
pub(crate) fn find_transcript(home: &Path, session_id: &str) -> Option<PathBuf> {
    let name = format!("{session_id}.jsonl");
    for config_dir in login_config_dirs(home) {
        let Ok(projects) = std::fs::read_dir(config_dir.join("projects")) else {
            continue;
        };
        for project in projects.flatten() {
            let candidate = project.path().join(&name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn login_config_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![home.join(".claude")];
    if let Ok(entries) = std::fs::read_dir(home) {
        let mut others = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                name.starts_with(".claude-").then(|| entry.path())
            })
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        others.sort();
        dirs.extend(others);
    }
    dirs
}

/// The logins this machine has: `claude` for `~/.claude` and `claude-<id>` for
/// each `~/.claude-<id>`, wherever credentials are stored.
pub(crate) fn discover_logins(home: &Path) -> Vec<String> {
    login_config_dirs(home)
        .into_iter()
        .filter(|dir| dir.join(".credentials.json").is_file())
        .filter_map(|dir| {
            let name = dir.file_name()?.to_str()?;
            Some(format!("claude{}", name.strip_prefix(".claude")?))
        })
        .collect()
}

/// The login a Claude resume command runs under: its program, when that is
/// `claude` or a `claude-<id>` wrapper.
pub(crate) fn login_of_argv(argv: &[String]) -> Option<&str> {
    let program = argv.first()?;
    let name = program.rsplit('/').next()?;
    (name == "claude" || name.starts_with("claude-")).then_some(name)
}

/// What a status line recorded of a session's rate limits
/// (`<login dir>/usage/<session>.json`, written from Claude Code's
/// `rate_limits`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct UsageRecord {
    pub(crate) recorded_at: u64,
    pub(crate) five_hour_percent: Option<u8>,
    pub(crate) five_hour_resets_at: Option<u64>,
    pub(crate) week_percent: Option<u8>,
}

pub(crate) fn read_usage(home: &Path, session_id: &str) -> Option<UsageRecord> {
    let name = format!("{session_id}.json");
    login_config_dirs(home)
        .into_iter()
        .filter_map(|dir| std::fs::read_to_string(dir.join("usage").join(&name)).ok())
        .filter_map(|text| parse_usage(&text))
        .max_by_key(|record| record.recorded_at)
}

fn parse_usage(text: &str) -> Option<UsageRecord> {
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    let percent = |window: &str| {
        value
            .pointer(&format!("/{window}/used_percentage"))
            .and_then(serde_json::Value::as_f64)
            .map(|percent| percent.clamp(0.0, 255.0) as u8)
    };
    Some(UsageRecord {
        recorded_at: value.get("recorded_at")?.as_u64()?,
        five_hour_percent: percent("five_hour"),
        five_hour_resets_at: value
            .pointer("/five_hour/resets_at")
            .and_then(serde_json::Value::as_f64)
            .map(|at| at as u64),
        week_percent: percent("seven_day"),
    })
}

/// One login as the relaunch picker shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct LoginStatus {
    pub(crate) login: String,
    /// Agents running under it now.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) agents: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) five_hour_percent: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) five_hour_resets_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) week_percent: Option<u8>,
    /// An agent under it hit a limit: when it lifts, or 0 when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) limited_until: Option<u64>,
    /// An agent under it was told to log in again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) login_expired: bool,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// A Claude session that exited in a pane and left the shell prompt after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExitedSession {
    pub(crate) session_id: String,
    /// The command that ran it, when the pane still shows it, else the resume
    /// command Claude printed.
    pub(crate) argv: Vec<String>,
}

/// Reads a pane's recent text for a Claude session that exited cleanly: its
/// "Resume this session with:" note, then nothing but an idle shell prompt.
/// A session that failed to start shows an error instead and is left alone.
pub(crate) fn exited_session(text: &str) -> Option<ExitedSession> {
    static RESUME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?:--resume|-r)\s+([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})",
        )
        .expect("resume pattern")
    });
    let lines = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    let (prompt, before) = lines.split_last()?;
    let prompt = prompt.trim();
    if !(prompt.ends_with('$') || prompt.ends_with('#') || prompt.ends_with('%')) {
        return None;
    }
    let printed = before.last()?;
    let note = before.get(before.len().checked_sub(2)?)?;
    if !note
        .trim()
        .eq_ignore_ascii_case("resume this session with:")
    {
        return None;
    }
    let session_id = RESUME.captures(printed)?.get(1)?.as_str().to_owned();
    let typed = before.iter().rev().skip(2).find_map(|line| {
        let (_, command) = line.rsplit_once("$ ")?;
        let argv = command
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        (login_of_argv(&argv).is_some() && command.contains(session_id.as_str())).then_some(argv)
    });
    let argv = typed.unwrap_or_else(|| {
        printed
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });
    login_of_argv(&argv)?;
    Some(ExitedSession { session_id, argv })
}

/// The newest title a transcript gives its session: one set with /rename, or
/// the one Claude generated.
pub(crate) fn transcript_title(path: &Path) -> Option<String> {
    let (text, whole) = read_tail(path, TAIL_BYTES)?;
    let mut lines = text.lines().collect::<Vec<_>>();
    if !whole && !lines.is_empty() {
        lines.remove(0);
    }
    let mut generated = None;
    for line in lines.iter().rev() {
        if !line.contains("-title") {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match entry.get("type").and_then(serde_json::Value::as_str) {
            Some("custom-title") => {
                if let Some(title) = entry.get("customTitle").and_then(serde_json::Value::as_str) {
                    return Some(title.to_owned());
                }
            }
            Some("ai-title") if generated.is_none() => {
                generated = entry
                    .get("aiTitle")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
            }
            _ => {}
        }
    }
    generated
}

/// What to do once a Claude agent asked to exit has gone (fork).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AfterAgentExit {
    /// Type this command into the shell the agent leaves.
    Relaunch(Vec<String>),
    /// Close the pane.
    Finish,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingAgentExit {
    pub(crate) then: AfterAgentExit,
    pub(crate) requested_at: std::time::Instant,
    pub(crate) exited_at: Option<std::time::Instant>,
    /// Enter was sent again because "/exit" stayed in the prompt.
    pub(crate) retried: bool,
}

/// What the server keeps per terminal about the Claude session in it.
#[derive(Debug, Clone, Default)]
pub(crate) struct AgentInsightState {
    /// The session the rest belongs to; a new session starts afresh.
    pub(crate) session_id: Option<String>,
    /// The transcript, as the session's hook reported it or as found by id.
    pub(crate) transcript_path: Option<PathBuf>,
    /// Size and modification time of the transcript when last read.
    pub(crate) stamp: Option<(u64, std::time::SystemTime)>,
    pub(crate) read_at: Option<std::time::Instant>,
    /// When the transcript was last looked for by session id.
    pub(crate) searched_at: Option<std::time::Instant>,
    /// Whether the clients were last told the limit holds.
    pub(crate) limit_shown: bool,
    pub(crate) insight: TranscriptInsight,
    pub(crate) usage: Option<UsageRecord>,
    pub(crate) usage_read_at: Option<std::time::Instant>,
    /// The uuid of a question the person chose not to answer.
    pub(crate) dismissed_question: Option<String>,
    /// What to send once the current limit lifts.
    pub(crate) continue_text: Option<String>,
}

impl AgentInsightState {
    /// The question to show: the last message asks and was not dismissed.
    pub(crate) fn open_question(&self) -> Option<&LastMessage> {
        self.insight
            .last_message
            .as_ref()
            .filter(|message| message.asks)
            .filter(|message| self.dismissed_question.as_deref() != Some(message.uuid.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(value: serde_json::Value) -> String {
        value.to_string()
    }

    fn reply(uuid: &str, text: &str, input: u64) -> String {
        line(serde_json::json!({
            "type": "assistant",
            "uuid": uuid,
            "timestamp": "2026-11-17T10:00:00.000Z",
            "message": {
                "model": "claude-opus-5-5",
                "content": [{"type": "text", "text": text}],
                "usage": {"input_tokens": input, "cache_read_input_tokens": 1000, "output_tokens": 10},
            },
        }))
    }

    fn prompt(text: &str) -> String {
        line(serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": text},
        }))
    }

    fn limit_error(text: &str) -> String {
        line(serde_json::json!({
            "type": "assistant",
            "isApiErrorMessage": true,
            "timestamp": "2026-11-17T10:00:00.000Z",
            "message": {"model": "<synthetic>", "content": [{"type": "text", "text": text}]},
        }))
    }

    fn tail(lines: &[String]) -> TranscriptInsight {
        parse_transcript_tail(&lines.join("\n"), false, 0)
    }

    #[test]
    fn a_reply_ending_in_a_question_asks_the_person() {
        let insight = tail(&[
            prompt("сделай"),
            reply(
                "u1",
                "Готово.\n\nВарианты: **A** или **B**. Какой берём?",
                5000,
            ),
            line(serde_json::json!({"type": "system", "subtype": "turn_duration"})),
            line(serde_json::json!({"type": "cost-state"})),
        ]);
        let message = insight.last_message.expect("last message");
        assert!(message.asks);
        assert_eq!(message.uuid, "u1");
        assert_eq!(
            message.excerpt,
            "Готово.\n\nВарианты: A или B. Какой берём?"
        );
        assert_eq!(insight.context_tokens, Some(6010));
        assert_eq!(insight.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(insight.limit, None);
    }

    #[test]
    fn a_recommendation_without_a_question_mark_still_asks() {
        assert!(asks_person(
            "Разбор выше.\n\n**Рекомендация:** A. Это обратимо одной командой."
        ));
        assert!(asks_person(
            "Пункт 3 (#1825) ждёт твоего ответа.\n\nЗа !1920 слежу."
        ));
        assert!(!asks_person(
            "!1915 влит. Main после !1918 зелёный, смотрю, что упало."
        ));
        assert!(!asks_person("Готово? Да.\n\nВсё влито, CI зелёный."));
        assert!(asks_person("Скажите A или B. Пока продолжаю пункты."));
        assert!(asks_person("Если подтвердишь, вливаю пачку одним заходом."));
    }

    /// Reports the first version of the rule took for questions, found in a
    /// sample of real turns.
    #[test]
    fn reports_that_ask_nothing_are_not_questions() {
        for report in [
            "Подтвердилось: пайплайн main завершился успешно. Жду остальные фоновые задачи.",
            "Флоту отдам, когда скажешь.\n\nТвоего решения сейчас ничего не ждёт.",
            "На вашей странице исправил описание: вопрос «Покинуть сайт?» работает только на компьютере.",
            "Порядок: #203 → #204. Если хотите другой порядок — скажите.",
            "С правами ассистента приду к тебе с одним вопросом и рекомендацией, когда будут факты.",
            "Ответ ему не нужен. Я жду своё фоновое наблюдение.",
            "Пример запроса: `GET /api/items?page=2` отдаёт вторую страницу.",
        ] {
            assert!(!asks_person(report), "{report}");
        }
    }

    #[test]
    fn a_prompt_after_the_reply_means_the_turn_is_not_waiting() {
        let insight = tail(&[reply("u1", "Какой берём?", 10), prompt("а")]);
        assert_eq!(insight.last_message, None);
        assert_eq!(insight.context_tokens, Some(1020));
    }

    #[test]
    fn a_tool_call_is_no_final_message() {
        let call = line(serde_json::json!({
            "type": "assistant",
            "uuid": "u2",
            "message": {"model": "claude-opus-5-5", "content": [{"type": "tool_use", "name": "Bash"}],
                        "usage": {"input_tokens": 7}},
        }));
        assert_eq!(
            tail(&[reply("u1", "Что делаем?", 1), call]).last_message,
            None
        );
    }

    #[test]
    fn a_compaction_hides_the_old_context_size() {
        let insight = tail(&[
            reply("u1", "Готово.", 300_000),
            line(serde_json::json!({"type": "system", "subtype": "compact_boundary"})),
            line(
                serde_json::json!({"type": "user", "isCompactSummary": true, "message": {"content": "summary"}}),
            ),
        ]);
        assert_eq!(insight.context_tokens, None);
        assert_eq!(insight.last_message, None);
        assert_eq!(insight.model.as_deref(), Some("claude-opus-5-5"));
    }

    #[test]
    fn the_newest_limit_error_holds_the_agent_until_a_prompt_follows() {
        // 10:00 UTC; 6:30pm at UTC+2 is 16:30 UTC.
        let error = limit_error("You've hit your 5-hour limit · resets 6:30pm (Europe/Riga)");
        let insight = parse_transcript_tail(
            &[reply("u1", "x", 1), error.clone()].join("\n"),
            false,
            7200,
        );
        let at = parse_iso_timestamp("2026-11-17T16:30:00.000Z").unwrap();
        assert_eq!(
            insight.limit,
            Some(LimitHit::Usage {
                resets_at: Some(at)
            })
        );
        let insight = tail(&[reply("u1", "x", 1), error, prompt("продолжай")]);
        assert_eq!(insight.limit, None);
    }

    #[test]
    fn reads_limit_messages() {
        let at = parse_iso_timestamp("2026-11-17T10:00:00.000Z").unwrap();
        let hit = |text| limit_from_text(text, Some(at), 7200);
        let utc = |value| Some(parse_iso_timestamp(value).unwrap());
        assert_eq!(
            hit("You've hit your 5-hour limit · resets 12am (Europe/Riga)"),
            Some(LimitHit::Usage {
                resets_at: utc("2026-11-17T22:00:00.000Z")
            })
        );
        assert_eq!(
            hit("You've hit your weekly limit · resets Nov 20, 3pm (Europe/Riga)"),
            Some(LimitHit::Usage {
                resets_at: utc("2026-11-20T13:00:00.000Z")
            })
        );
        // 11:00 local has passed at 12:00 local: tomorrow.
        assert_eq!(
            hit("You've hit your session limit · resets 11am"),
            Some(LimitHit::Usage {
                resets_at: utc("2026-11-18T09:00:00.000Z")
            })
        );
        assert_eq!(
            hit("Claude AI usage limit reached|1794913200"),
            Some(LimitHit::Usage {
                resets_at: Some(1_794_913_200)
            })
        );
        assert_eq!(
            hit("Login reached its time limit · Please run /login"),
            Some(LimitHit::Login)
        );
        assert_eq!(
            hit("API Error: Can't reach the API server — check your internet or DNS (ENOTFOUND)"),
            None
        );
    }

    #[test]
    fn a_long_last_paragraph_keeps_its_end() {
        let text = format!("start {}", "слово ".repeat(300));
        let excerpt = excerpt(&text);
        assert!(excerpt.starts_with('…'));
        assert!(excerpt.chars().count() <= EXCERPT_CHARS + 1);
        assert!(excerpt.ends_with("слово"));
    }

    #[test]
    fn finds_a_cleanly_exited_session() {
        let text = "arthur@host:/srv$ claude-cc --dangerously-skip-permissions --resume 708adf44-b806-4086-be41-b3de3eca78fb\n\
            ╭ some tui output\n\
            \n\
            Resume this session with:\n\
            claude --effort high --dangerously-skip-permissions --resume 708adf44-b806-4086-be41-b3de3eca78fb\n\
            arthur@host:/srv$ \n\n";
        assert_eq!(
            exited_session(text),
            Some(ExitedSession {
                session_id: "708adf44-b806-4086-be41-b3de3eca78fb".into(),
                argv: vec![
                    "claude-cc".into(),
                    "--dangerously-skip-permissions".into(),
                    "--resume".into(),
                    "708adf44-b806-4086-be41-b3de3eca78fb".into(),
                ],
            })
        );
        // Typing after the prompt, or no resume note: not an exited session.
        assert_eq!(
            exited_session(&text.replace("/srv$ \n\n", "/srv$ ls")),
            None
        );
        assert_eq!(
            exited_session("arthur@host:/srv$ claude --resume 708adf44-b806-4086-be41-b3de3eca78fb\nError: EACCES\narthur@host:/srv$"),
            None
        );
        // Only the printed command: it is used as is.
        let printed = "Resume this session with:\nclaude --resume 708adf44-b806-4086-be41-b3de3eca78fb\nuser@h:~$";
        assert_eq!(
            exited_session(printed).map(|session| session.argv),
            Some(vec![
                "claude".into(),
                "--resume".into(),
                "708adf44-b806-4086-be41-b3de3eca78fb".into()
            ])
        );
    }

    #[test]
    fn limit_tokens_round_trip() {
        for hit in [
            LimitHit::Login,
            LimitHit::Usage { resets_at: None },
            LimitHit::Usage {
                resets_at: Some(42),
            },
        ] {
            assert_eq!(LimitHit::from_token(&hit.token_value()), Some(hit));
        }
        assert!(LimitHit::Usage {
            resets_at: Some(42)
        }
        .holds_at(41));
        assert!(!LimitHit::Usage {
            resets_at: Some(42)
        }
        .holds_at(42));
    }

    struct TempHome(PathBuf);

    impl TempHome {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "herdr-insight-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn finds_logins_and_usage() {
        let home = TempHome::new("logins");
        for dir in [".claude", ".claude-cc", ".claude-aija-w4", ".claude-old"] {
            std::fs::create_dir_all(home.path().join(dir)).unwrap();
        }
        for dir in [".claude", ".claude-cc", ".claude-aija-w4"] {
            std::fs::write(home.path().join(dir).join(".credentials.json"), "{}").unwrap();
        }
        assert_eq!(
            discover_logins(home.path()),
            ["claude", "claude-aija-w4", "claude-cc"]
        );
        let usage = home.path().join(".claude-cc/usage");
        std::fs::create_dir_all(&usage).unwrap();
        std::fs::write(
            usage.join("s1.json"),
            r#"{"recorded_at":100,"five_hour":{"used_percentage":91.6,"resets_at":2000},"seven_day":{"used_percentage":42}}"#,
        )
        .unwrap();
        assert_eq!(
            read_usage(home.path(), "s1"),
            Some(UsageRecord {
                recorded_at: 100,
                five_hour_percent: Some(91),
                five_hour_resets_at: Some(2000),
                week_percent: Some(42),
            })
        );
        let projects = home.path().join(".claude/projects/-var-www");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(projects.join("s1.jsonl"), "").unwrap();
        assert_eq!(
            find_transcript(home.path(), "s1"),
            Some(projects.join("s1.jsonl"))
        );
        assert_eq!(
            login_of_argv(&["claude-cc".into(), "--resume".into()]),
            Some("claude-cc")
        );
        assert_eq!(login_of_argv(&["codex".into()]), None);
    }
}
