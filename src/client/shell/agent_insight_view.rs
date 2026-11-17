//! What the server tells about a Claude agent through reserved tokens (see
//! `crate::agent_insight`): whether its last message asks the person, a limit
//! that holds it, how full its context is, its login and model.

use crate::agent_insight::{self as insight, LimitHit, LoginStatus};
use crate::protocol::ClientShellAgent;

/// From this many tokens in context the row shows the count.
pub(super) const CONTEXT_SHOWN_FROM: u64 = 200_000;
/// From this many "Compact agents over 250k" takes the agent.
pub(super) const CONTEXT_LARGE_FROM: u64 = 250_000;
/// From this many the count stands out.
pub(super) const CONTEXT_WARN_FROM: u64 = 350_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct AgentInsightView {
    /// The server reports insight for this agent (a Claude session).
    pub(super) known: bool,
    pub(super) asks: bool,
    pub(super) limit: Option<LimitHit>,
    pub(super) continue_after_reset: bool,
    pub(super) context: Option<u64>,
    pub(super) model: Option<String>,
    pub(super) login: Option<String>,
    pub(super) excerpt: Option<String>,
    pub(super) resume_argv: Option<Vec<String>>,
}

impl AgentInsightView {
    pub(super) fn of(agent: &ClientShellAgent) -> Self {
        Self::at(agent, crate::pane_todo::unix_now())
    }

    pub(super) fn at(agent: &ClientShellAgent, now: u64) -> Self {
        let token = |name: &str| {
            agent
                .tokens
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        let working = agent.agent_status == crate::api::schema::AgentStatus::Working;
        Self {
            known: token(insight::LOGINS_TOKEN).is_some(),
            asks: !working && token(insight::QUESTION_TOKEN).is_some(),
            limit: token(insight::LIMIT_TOKEN)
                .and_then(LimitHit::from_token)
                .filter(|hit| !working && hit.holds_at(now)),
            continue_after_reset: token(insight::CONTINUE_TOKEN).is_some(),
            context: token(insight::CONTEXT_TOKEN).and_then(|value| value.parse().ok()),
            model: token(insight::MODEL_TOKEN).map(str::to_owned),
            login: token(insight::LOGIN_TOKEN).map(str::to_owned),
            excerpt: token(insight::EXCERPT_TOKEN).map(str::to_owned),
            resume_argv: token(insight::RESUME_TOKEN)
                .and_then(|value| serde_json::from_str(value).ok()),
        }
    }

    /// The agent waits with a large context: worth a /compact.
    pub(super) fn compactable(&self) -> bool {
        self.context
            .is_some_and(|tokens| tokens >= CONTEXT_LARGE_FROM)
    }
}

/// What a stopped agent waits for beyond its state: an answer to the question
/// its last message asks, or a usage limit to lift.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Attention {
    #[default]
    None,
    Question,
    Limit,
}

impl Attention {
    /// Reads only the two tokens it needs: the lists sort by it often.
    pub(super) fn of(agent: &ClientShellAgent) -> Self {
        if agent.agent_status == crate::api::schema::AgentStatus::Working {
            return Self::None;
        }
        let mut asks = false;
        for (key, value) in &agent.tokens {
            if key == insight::LIMIT_TOKEN
                && LimitHit::from_token(value)
                    .is_some_and(|hit| hit.holds_at(crate::pane_todo::unix_now()))
            {
                return Self::Limit;
            }
            asks |= key == insight::QUESTION_TOKEN;
        }
        if asks {
            Self::Question
        } else {
            Self::None
        }
    }
}

/// The logins and what is known of them, as any Claude agent's tokens carry.
pub(super) fn login_statuses(agents: &[ClientShellAgent]) -> Vec<LoginStatus> {
    agents
        .iter()
        .find_map(|agent| {
            agent
                .tokens
                .iter()
                .find(|(key, _)| key == insight::LOGINS_TOKEN)
                .and_then(|(_, value)| serde_json::from_str(value).ok())
        })
        .unwrap_or_default()
}

/// "212k", "1.2M".
pub(super) fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else {
        format!("{}k", tokens / 1000)
    }
}

/// "claude-cc" → "cc", "claude" stays.
pub(super) fn short_login(login: &str) -> &str {
    login.strip_prefix("claude-").unwrap_or(login)
}

/// "claude-opus-5-5" → "opus 5.5".
pub(super) fn short_model(model: &str) -> String {
    let model = model.strip_prefix("claude-").unwrap_or(model);
    let mut parts = model.split('-').collect::<Vec<_>>();
    // Date suffixes ("20250514") carry nothing a person picks by.
    parts.retain(|part| !(part.len() >= 8 && part.chars().all(|c| c.is_ascii_digit())));
    match parts.split_first() {
        Some((family, version)) if !version.is_empty() => {
            format!("{family} {}", version.join("."))
        }
        _ => model.to_owned(),
    }
}

/// When a limit lifts, as the row and the card show it: "18:30", "Thu 15:00",
/// "login" for an expired login, "?" when unknown.
pub(super) fn limit_label(hit: LimitHit) -> String {
    match hit {
        LimitHit::Usage {
            resets_at: Some(at),
        } => crate::pane_todo::format_remind_at(
            at,
            crate::pane_todo::unix_now(),
            crate::pane_todo::cached_local_utc_offset_secs(),
        ),
        LimitHit::Usage { resets_at: None } => "?".to_owned(),
        LimitHit::Login => "login".to_owned(),
    }
}

/// A login as the relaunch menu shows it: "cc   5h 92% · 18:30 · wk 42%",
/// or "cc   limit until 18:30".
pub(super) fn login_menu_label(status: &LoginStatus, current: Option<&str>) -> String {
    let now = crate::pane_todo::unix_now();
    let offset = crate::pane_todo::cached_local_utc_offset_secs();
    let mut parts = Vec::new();
    if status.login_expired {
        parts.push("login expired".to_owned());
    } else if let Some(until) = status
        .limited_until
        .filter(|until| *until == 0 || *until > now)
    {
        parts.push(if until == 0 {
            "limit hit".to_owned()
        } else {
            format!(
                "limit until {}",
                crate::pane_todo::format_remind_at(until, now, offset)
            )
        });
    } else if let Some(percent) = status.five_hour_percent {
        let mut usage = format!("5h {percent}%");
        if let Some(at) = status.five_hour_resets_at.filter(|at| *at > now) {
            usage.push_str(&format!(
                " · {}",
                crate::pane_todo::format_remind_at(at, now, offset)
            ));
        }
        parts.push(usage);
    }
    if let Some(percent) = status.week_percent {
        parts.push(format!("wk {percent}%"));
    }
    let mut label = short_login(&status.login).to_owned();
    if current == Some(status.login.as_str()) {
        label.push_str(" (now)");
    }
    if !parts.is_empty() {
        let pad = 14usize.saturating_sub(label.chars().count()).max(1);
        label.push_str(&" ".repeat(pad));
        label.push_str(&parts.join(" · "));
    }
    label
}

/// Logins in the order the relaunch menu offers them: free ones by least
/// used, then those at a limit, the agent's own login left out.
pub(super) fn relaunch_choices(
    statuses: &[LoginStatus],
    current: Option<&str>,
) -> Vec<LoginStatus> {
    let now = crate::pane_todo::unix_now();
    let mut choices = statuses
        .iter()
        .filter(|status| Some(status.login.as_str()) != current)
        .cloned()
        .collect::<Vec<_>>();
    choices.sort_by_key(|status| {
        let blocked = status.login_expired
            || status
                .limited_until
                .is_some_and(|until| until == 0 || until > now);
        (
            blocked,
            status.five_hour_percent.unwrap_or(u8::MAX / 2),
            status.login.clone(),
        )
    });
    choices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortens_names() {
        assert_eq!(short_model("claude-opus-5-5"), "opus 5.5");
        assert_eq!(short_model("claude-sonnet-4-5-20250929"), "sonnet 4.5");
        assert_eq!(short_login("claude-aija-w4"), "aija-w4");
        assert_eq!(short_login("claude"), "claude");
        assert_eq!(format_tokens(212_345), "212k");
        assert_eq!(format_tokens(1_250_000), "1.2M");
    }

    #[test]
    fn free_logins_come_first() {
        let status = |login: &str, percent: Option<u8>, limited: Option<u64>| LoginStatus {
            login: login.into(),
            five_hour_percent: percent,
            limited_until: limited,
            ..LoginStatus::default()
        };
        let choices = relaunch_choices(
            &[
                status("claude", Some(10), None),
                status("claude-cc", Some(100), Some(u64::MAX)),
                status("claude-per", Some(60), None),
                status("claude-aija-w4", None, None),
            ],
            Some("claude"),
        );
        let order = choices
            .iter()
            .map(|status| status.login.as_str())
            .collect::<Vec<_>>();
        assert_eq!(order, ["claude-per", "claude-aija-w4", "claude-cc"]);
    }
}
