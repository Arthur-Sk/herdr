//! Keeps what each Claude terminal's transcript says fresh (see
//! `crate::agent_insight`): the server wakes every couple of seconds while a
//! Claude agent runs, rereads only transcripts that changed, and reports a
//! change so clients get a new snapshot.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::App;
use crate::agent_insight::{AgentInsightState, LoginStatus};

const REFRESH_EVERY: Duration = Duration::from_secs(2);
/// While an asked-for exit or a continue waits, the server checks this often.
const FOLLOWUP_EVERY: Duration = Duration::from_millis(250);
/// A working agent's transcript grows all the time; read it at most this often.
const WORKING_REREAD: Duration = Duration::from_secs(5);
/// How long a session whose transcript was not found waits for the next search.
const SEARCH_AGAIN: Duration = Duration::from_secs(30);
const USAGE_REREAD: Duration = Duration::from_secs(30);
const LOGINS_REREAD: Duration = Duration::from_secs(60);

#[derive(Debug, Default)]
pub(crate) struct AgentInsightTimers {
    pub(crate) next_refresh: Option<Instant>,
    last_reread: Option<Instant>,
    logins: Vec<String>,
    logins_read_at: Option<Instant>,
}

impl App {
    /// The transcript a Claude session's hook reported for this pane.
    pub(crate) fn note_agent_transcript(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        session_id: &str,
        path: PathBuf,
    ) {
        let Some(terminal) = self.terminal_of_pane_mut(ws_idx, pane_id) else {
            return;
        };
        let state = &mut terminal.insight;
        if state.session_id.as_deref() != Some(session_id) {
            *state = AgentInsightState {
                session_id: Some(session_id.to_owned()),
                ..AgentInsightState::default()
            };
        }
        if state.transcript_path.as_ref() != Some(&path) {
            state.transcript_path = Some(path);
            state.stamp = None;
        }
        self.agent_insight.next_refresh = Some(Instant::now());
    }

    pub(crate) fn terminal_of_pane_mut(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<&mut crate::terminal::TerminalState> {
        let terminal_id = self
            .state
            .workspaces
            .get(ws_idx)?
            .terminal_id(pane_id)?
            .clone();
        self.state.terminals.get_mut(&terminal_id)
    }

    pub(crate) fn agent_insight_deadline(&self) -> Option<Instant> {
        let at = self.agent_insight.next_refresh?;
        // The first cycle waits for a Claude agent: a server without one has
        // nothing to wake for.
        (self.agent_insight.last_reread.is_some()
            || self
                .state
                .terminals
                .values()
                .any(|terminal| terminal.effective_agent_label() == Some("claude")))
        .then_some(at)
    }

    /// Starts the refresh cycle; later cycles schedule themselves while a
    /// Claude agent runs.
    pub(crate) fn schedule_agent_insight_refresh(&mut self) {
        self.agent_insight
            .next_refresh
            .get_or_insert_with(Instant::now);
    }

    /// Rereads changed transcripts and usage records. True when something a
    /// client shows changed.
    pub(crate) fn refresh_agent_insights(&mut self, now: Instant) -> bool {
        if self.agent_insight_deadline().is_none_or(|at| now < at) {
            return false;
        }
        let mut changed = self.run_agent_followups(now);
        let followups = self.has_agent_followups();
        if self
            .agent_insight
            .last_reread
            .is_some_and(|at| now.duration_since(at) < REFRESH_EVERY)
        {
            self.agent_insight.next_refresh = Some(now + FOLLOWUP_EVERY);
            return changed;
        }
        self.agent_insight.last_reread = Some(now);
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            self.agent_insight.next_refresh = None;
            return false;
        };
        let utc_offset = crate::pane_todo::cached_local_utc_offset_secs();
        let unix_now = crate::pane_todo::unix_now();
        let mut any_claude = false;
        for terminal in self.state.terminals.values_mut() {
            let session_id = (terminal.effective_agent_label() == Some("claude"))
                .then(|| terminal.claude_session_id())
                .flatten();
            let Some(session_id) = session_id else {
                if terminal.insight.session_id.is_some() {
                    terminal.insight = AgentInsightState::default();
                    changed = true;
                }
                continue;
            };
            any_claude = true;
            let working = terminal.state == crate::detect::AgentState::Working;
            changed |= refresh_terminal(
                &mut terminal.insight,
                &home,
                &session_id,
                working,
                now,
                unix_now,
                utc_offset,
            );
        }
        if self
            .agent_insight
            .logins_read_at
            .is_none_or(|at| now.duration_since(at) >= LOGINS_REREAD)
        {
            self.agent_insight.logins_read_at = Some(now);
            let logins = crate::agent_insight::discover_logins(&home);
            if logins != self.agent_insight.logins {
                self.agent_insight.logins = logins;
                changed = true;
            }
        }
        self.agent_insight.next_refresh = if followups {
            Some(now + FOLLOWUP_EVERY)
        } else {
            any_claude.then_some(now + REFRESH_EVERY)
        };
        changed
    }

    /// The logins a Claude agent can be relaunched under, with what is known of
    /// their limits from the agents running under them now.
    pub(crate) fn claude_login_statuses(&self) -> Vec<LoginStatus> {
        let now = crate::pane_todo::unix_now();
        let mut statuses = self
            .agent_insight
            .logins
            .iter()
            .map(|login| LoginStatus {
                login: login.clone(),
                ..LoginStatus::default()
            })
            .collect::<Vec<_>>();
        for terminal in self.state.terminals.values() {
            if terminal.insight.session_id.is_none() {
                continue;
            }
            let Some(login) = terminal_login(terminal) else {
                continue;
            };
            let Some(status) = statuses.iter_mut().find(|status| status.login == login) else {
                continue;
            };
            status.agents += 1;
            if let Some(usage) = terminal.insight.usage {
                let newer = status
                    .five_hour_resets_at
                    .is_none_or(|_| usage.five_hour_percent >= status.five_hour_percent);
                if newer {
                    status.five_hour_percent = usage.five_hour_percent;
                    status.five_hour_resets_at = usage.five_hour_resets_at;
                    status.week_percent = usage.week_percent.or(status.week_percent);
                }
            }
            match terminal.insight.insight.limit {
                Some(crate::agent_insight::LimitHit::Login) => status.login_expired = true,
                Some(hit @ crate::agent_insight::LimitHit::Usage { resets_at })
                    if hit.holds_at(now) =>
                {
                    let until = resets_at.unwrap_or(0);
                    status.limited_until =
                        Some(status.limited_until.map_or(until, |known| known.max(until)));
                }
                _ => {}
            }
        }
        statuses
    }

    /// The reserved tokens a Claude agent's row carries (see
    /// `crate::agent_insight`).
    pub(crate) fn agent_insight_tokens(
        &self,
        terminal: &crate::terminal::TerminalState,
        logins_json: &str,
    ) -> Vec<(String, String)> {
        use crate::agent_insight as insight;

        let state = &terminal.insight;
        if state.session_id.is_none() {
            return Vec::new();
        }
        let mut tokens = vec![(insight::LOGINS_TOKEN.to_owned(), logins_json.to_owned())];
        let found = &state.insight;
        if let Some(message) = &found.last_message {
            tokens.push((insight::EXCERPT_TOKEN.to_owned(), message.excerpt.clone()));
        }
        if state.open_question().is_some() {
            tokens.push((insight::QUESTION_TOKEN.to_owned(), "1".to_owned()));
        }
        if let Some(context) = found.context_tokens {
            tokens.push((insight::CONTEXT_TOKEN.to_owned(), context.to_string()));
        }
        if let Some(model) = &found.model {
            tokens.push((insight::MODEL_TOKEN.to_owned(), model.clone()));
        }
        if let Some(hit) = found
            .limit
            .filter(|hit| hit.holds_at(crate::pane_todo::unix_now()))
        {
            let hit = match hit {
                crate::agent_insight::LimitHit::Usage { resets_at: None } => {
                    crate::agent_insight::LimitHit::Usage {
                        resets_at: state.usage.and_then(|usage| usage.five_hour_resets_at),
                    }
                }
                hit => hit,
            };
            tokens.push((insight::LIMIT_TOKEN.to_owned(), hit.token_value()));
            if state.continue_text.is_some() {
                tokens.push((insight::CONTINUE_TOKEN.to_owned(), "1".to_owned()));
            }
        }
        if let Some(resume) = terminal.reported_resume() {
            if let Some(login) = crate::agent_insight::login_of_argv(&resume.argv) {
                tokens.push((insight::LOGIN_TOKEN.to_owned(), login.to_owned()));
            }
            if let Ok(argv) = serde_json::to_string(&resume.argv) {
                tokens.push((insight::RESUME_TOKEN.to_owned(), argv));
            }
        }
        tokens
    }
}

pub(crate) fn terminal_login(terminal: &crate::terminal::TerminalState) -> Option<&str> {
    terminal
        .reported_resume()
        .and_then(|resume| crate::agent_insight::login_of_argv(&resume.argv))
}

fn refresh_terminal(
    state: &mut AgentInsightState,
    home: &Path,
    session_id: &str,
    working: bool,
    now: Instant,
    unix_now: u64,
    utc_offset: i64,
) -> bool {
    let mut changed = false;
    if state.session_id.as_deref() != Some(session_id) {
        *state = AgentInsightState {
            session_id: Some(session_id.to_owned()),
            ..AgentInsightState::default()
        };
        changed = true;
    }
    let path_gone = state
        .transcript_path
        .as_ref()
        .is_none_or(|path| !path.is_file());
    if path_gone
        && state
            .searched_at
            .is_none_or(|at| now.duration_since(at) >= SEARCH_AGAIN)
    {
        state.searched_at = Some(now);
        state.transcript_path = crate::agent_insight::find_transcript(home, session_id);
        state.stamp = None;
    }
    if let Some(path) = state.transcript_path.clone() {
        let stamp = std::fs::metadata(&path).ok().map(|metadata| {
            (
                metadata.len(),
                metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
            )
        });
        let due = stamp != state.stamp
            && (!working
                || state
                    .read_at
                    .is_none_or(|at| now.duration_since(at) >= WORKING_REREAD));
        if due {
            if let Some(insight) = crate::agent_insight::read_transcript_insight(&path, utc_offset)
            {
                state.stamp = stamp;
                state.read_at = Some(now);
                if insight != state.insight {
                    state.insight = insight;
                    changed = true;
                }
            }
        }
    }
    if state
        .usage_read_at
        .is_none_or(|at| now.duration_since(at) >= USAGE_REREAD)
    {
        state.usage_read_at = Some(now);
        let usage = crate::agent_insight::read_usage(home, session_id);
        if usage != state.usage {
            state.usage = usage;
            changed = true;
        }
    }
    // A limit that just lifted changes the row even with nothing reread.
    let limit_holds = state
        .insight
        .limit
        .is_some_and(|hit| hit.holds_at(unix_now));
    if limit_holds != state.limit_shown {
        state.limit_shown = limit_holds;
        changed = true;
    }
    changed
}
