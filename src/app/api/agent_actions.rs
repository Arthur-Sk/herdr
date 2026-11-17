//! Fork: acting on Claude agents from the sidebar. `pane.agent.act` dismisses a
//! question, relaunches an agent under another login, sends a prompt once a
//! usage limit lifts, submits a command, or finishes an agent and closes its
//! pane; `agent.sweep` closes panes that exited sessions left behind and
//! `agent.reopen` opens such an agent again.
//!
//! Typing into Claude is done only while it waits with an empty prompt, so a
//! draft the person left there is never sent along.

use std::time::{Duration, Instant};

use bytes::Bytes;

use crate::agent_insight::{AfterAgentExit, PendingAgentExit};
use crate::api::schema::{
    AgentReopenParams, AgentSweepParams, ClosedAgentInfo, PaneAgentActParams, PaneAgentAction,
    PaneTarget, ResponseResult, TabCreateParams,
};
use crate::app::App;

use super::responses::{encode_error, encode_success};

/// How long an asked-for exit may take before the follow-up is dropped.
const EXIT_GIVE_UP: Duration = Duration::from_secs(20);
/// When "/exit" is still in the prompt this long after Enter, Enter again.
const EXIT_RETRY: Duration = Duration::from_millis(1500);
/// The shell gets this long after the agent leaves before input reaches it.
const AFTER_EXIT: Duration = Duration::from_millis(400);
/// A limit is given this long past its reset before the continue prompt.
const CONTINUE_GRACE_SECS: u64 = 60;
const DEFAULT_CONTINUE_TEXT: &str = "continue";
/// Lines of a pane's recent text read for an exited session.
const SWEEP_LINES: usize = 60;

impl App {
    pub(crate) fn handle_pane_agent_act(
        &mut self,
        id: String,
        params: PaneAgentActParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return not_found(id, &params.pane_id);
        };
        let Some(terminal) = self.terminal_of_pane_mut(ws_idx, pane_id) else {
            return not_found(id, &params.pane_id);
        };
        if terminal.effective_agent_label() != Some("claude") {
            return encode_error(id, "not_claude", "no Claude agent runs in this pane");
        }
        match params.action {
            PaneAgentAction::DismissQuestion => {
                terminal.insight.dismissed_question = terminal
                    .insight
                    .insight
                    .last_message
                    .as_ref()
                    .map(|message| message.uuid.clone());
                encode_success(id, ResponseResult::Ok {})
            }
            PaneAgentAction::ContinueAfterReset => {
                terminal.insight.continue_text = params.on.unwrap_or(true).then(|| {
                    params
                        .text
                        .filter(|text| !text.trim().is_empty())
                        .unwrap_or_else(|| DEFAULT_CONTINUE_TEXT.to_owned())
                });
                self.schedule_agent_insight_refresh();
                encode_success(id, ResponseResult::Ok {})
            }
            PaneAgentAction::Send => {
                let Some(text) = params.text.filter(|text| !text.trim().is_empty()) else {
                    return encode_error(id, "empty_text", "nothing to send");
                };
                match self.submit_to_waiting_claude(ws_idx, pane_id, &text) {
                    Ok(()) => encode_success(id, ResponseResult::Ok {}),
                    Err((code, message)) => encode_error(id, code, message),
                }
            }
            PaneAgentAction::Relaunch => {
                let Some(login) = params.login else {
                    return encode_error(id, "missing_login", "relaunch needs a login");
                };
                if !self.claude_login_known(&login) {
                    return encode_error(id, "unknown_login", format!("no login {login} here"));
                }
                let Some(argv) = self.claude_resume_argv(ws_idx, pane_id) else {
                    return encode_error(
                        id,
                        "no_resume_command",
                        "the agent reported no command that resumes it",
                    );
                };
                let mut argv = argv;
                argv[0] = login;
                match self.exit_claude(ws_idx, pane_id, AfterAgentExit::Relaunch(argv)) {
                    Ok(()) => encode_success(id, ResponseResult::Ok {}),
                    Err((code, message)) => encode_error(id, code, message),
                }
            }
            PaneAgentAction::Finish => {
                let Some(argv) = self.claude_resume_argv(ws_idx, pane_id) else {
                    return encode_error(
                        id,
                        "no_resume_command",
                        "the agent reported no command that resumes it",
                    );
                };
                let record = self.closed_agent_info(ws_idx, pane_id, argv);
                match self.exit_claude(ws_idx, pane_id, AfterAgentExit::Finish) {
                    Ok(()) => encode_success(
                        id,
                        ResponseResult::ClosedAgents {
                            agents: record.into_iter().collect(),
                        },
                    ),
                    Err((code, message)) => encode_error(id, code, message),
                }
            }
        }
    }

    pub(crate) fn handle_agent_sweep(&mut self, id: String, params: AgentSweepParams) -> String {
        let mut found = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for pane_id in tab.layout.pane_ids() {
                    let Some(terminal_id) = tab.terminal_id(pane_id) else {
                        continue;
                    };
                    let Some(terminal) = self.state.terminals.get(terminal_id) else {
                        continue;
                    };
                    if terminal.detected_agent.is_some() || terminal.pending_agent_exit.is_some() {
                        continue;
                    }
                    let Some(runtime) = self.terminal_runtimes.get(terminal_id) else {
                        continue;
                    };
                    let text = runtime.recent_unwrapped_text_snapshot(SWEEP_LINES).text;
                    if let Some(exited) = crate::agent_insight::exited_session(&text) {
                        found.push((ws_idx, pane_id, exited));
                    }
                }
            }
        }
        let mut closed = Vec::new();
        for (ws_idx, pane_id, exited) in found {
            let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
                continue;
            };
            if self.state.close_pane_would_close_workspace(ws_idx, pane_id) {
                continue;
            }
            let Some(mut record) = self.closed_agent_info(ws_idx, pane_id, exited.argv) else {
                continue;
            };
            if record.title.is_empty() {
                record.title = transcript_title_of(&exited.session_id)
                    .unwrap_or_else(|| format!("claude {}", &exited.session_id[..8]));
            }
            if !params.dry_run {
                // Ids of later panes stay valid: each is parsed again.
                let target = PaneTarget {
                    pane_id: public_pane_id,
                };
                if self.close_pane(id.clone(), &target).is_err() {
                    continue;
                }
            }
            closed.push(record);
        }
        encode_success(id, ResponseResult::ClosedAgents { agents: closed })
    }

    pub(crate) fn handle_agent_reopen(&mut self, id: String, params: AgentReopenParams) -> String {
        if params.argv.is_empty() || crate::agent_insight::login_of_argv(&params.argv).is_none() {
            return encode_error(id, "invalid_argv", "only a Claude command can be reopened");
        }
        let Some(command) = super::super::agent_resume::shell_command_from_argv(&params.argv)
        else {
            return encode_error(id, "invalid_argv", "empty command");
        };
        let workspace_id = params
            .workspace_id
            .filter(|workspace_id| self.parse_workspace_id(workspace_id).is_some());
        let response = self.handle_tab_create(
            id.clone(),
            TabCreateParams {
                workspace_id,
                cwd: std::path::Path::new(&params.cwd)
                    .is_dir()
                    .then(|| params.cwd.clone()),
                focus: true,
                label: params.label,
                env: Default::default(),
            },
        );
        let root_pane = serde_json::from_str::<serde_json::Value>(&response)
            .ok()
            .and_then(|value| {
                value
                    .pointer("/result/root_pane/pane_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            });
        let Some((ws_idx, pane_id)) = root_pane.and_then(|pane_id| self.parse_pane_id(&pane_id))
        else {
            return response;
        };
        if let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) {
            let mut input = command;
            input.push('\r');
            if let Err(err) = runtime.try_send_bytes(Bytes::from(input)) {
                return encode_error(id, "agent_reopen_failed", err.to_string());
            }
        }
        response
    }

    /// Finishes exits the person asked for: relaunches or closes once the
    /// agent is gone, gives up when it does not go. Also sends the continue
    /// prompt to agents whose limit lifted. True when something changed.
    pub(crate) fn run_agent_followups(&mut self, now: Instant) -> bool {
        let mut changed = false;
        let mut due = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for pane_id in tab.layout.pane_ids() {
                    let Some(terminal) = tab
                        .terminal_id(pane_id)
                        .and_then(|terminal_id| self.state.terminals.get(terminal_id))
                    else {
                        continue;
                    };
                    if terminal.pending_agent_exit.is_some()
                        || terminal.insight.continue_text.is_some()
                    {
                        due.push((ws_idx, pane_id));
                    }
                }
            }
        }
        for (ws_idx, pane_id) in due {
            changed |= self.run_pending_exit(ws_idx, pane_id, now);
            changed |= self.run_continue_after_reset(ws_idx, pane_id);
        }
        changed
    }

    pub(crate) fn has_agent_followups(&self) -> bool {
        self.state.terminals.values().any(|terminal| {
            terminal.pending_agent_exit.is_some() || terminal.insight.continue_text.is_some()
        })
    }

    fn run_pending_exit(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        now: Instant,
    ) -> bool {
        let Some(terminal) = self.terminal_of_pane_mut(ws_idx, pane_id) else {
            return false;
        };
        let Some(pending) = terminal.pending_agent_exit.as_mut() else {
            return false;
        };
        if terminal.detected_agent.is_some() {
            if now.duration_since(pending.requested_at) >= EXIT_GIVE_UP {
                tracing::warn!(
                    pane = pane_id.raw(),
                    "agent did not exit; follow-up dropped"
                );
                terminal.pending_agent_exit = None;
                return true;
            }
            if !pending.retried && now.duration_since(pending.requested_at) >= EXIT_RETRY {
                pending.retried = true;
                self.press_enter_if_prompt_holds(ws_idx, pane_id, "/exit");
            }
            return false;
        }
        let exited_at = *pending.exited_at.get_or_insert(now);
        if now.duration_since(exited_at) < AFTER_EXIT {
            return false;
        }
        let then = pending.then.clone();
        terminal.pending_agent_exit = None;
        match then {
            AfterAgentExit::Relaunch(argv) => {
                let Some(mut input) = super::super::agent_resume::shell_command_from_argv(&argv)
                else {
                    return true;
                };
                input.push('\r');
                if let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) {
                    if let Err(err) = runtime.try_send_bytes(Bytes::from(input)) {
                        tracing::warn!(err = %err, "failed to relaunch agent");
                    }
                }
            }
            AfterAgentExit::Finish => {
                if self.state.close_pane_would_close_workspace(ws_idx, pane_id) {
                    return true;
                }
                if let Some(pane_id) = self.public_pane_id(ws_idx, pane_id) {
                    let _ = self.close_pane("agent-finish".to_owned(), &PaneTarget { pane_id });
                }
            }
        }
        true
    }

    fn run_continue_after_reset(&mut self, ws_idx: usize, pane_id: crate::layout::PaneId) -> bool {
        let now = crate::pane_todo::unix_now();
        let Some(terminal) = self.terminal_of_pane_mut(ws_idx, pane_id) else {
            return false;
        };
        let Some(text) = terminal.insight.continue_text.clone() else {
            return false;
        };
        let resets_at = match terminal.insight.insight.limit {
            Some(crate::agent_insight::LimitHit::Usage { resets_at }) => resets_at.or_else(|| {
                terminal
                    .insight
                    .usage
                    .and_then(|usage| usage.five_hour_resets_at)
            }),
            // The person wrote, or the login itself expired: nothing to wait for.
            _ => {
                terminal.insight.continue_text = None;
                return true;
            }
        };
        let Some(resets_at) = resets_at else {
            return false;
        };
        if now < resets_at + CONTINUE_GRACE_SECS
            || terminal.state != crate::detect::AgentState::Idle
        {
            return false;
        }
        terminal.insight.continue_text = None;
        if let Err((_, message)) = self.submit_to_waiting_claude(ws_idx, pane_id, &text) {
            tracing::warn!(pane = pane_id.raw(), %message, "could not continue after limit");
        }
        true
    }

    /// Types `text` into a Claude agent that waits with an empty prompt and
    /// submits it.
    fn submit_to_waiting_claude(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        text: &str,
    ) -> Result<(), (&'static str, String)> {
        let Some(terminal) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane_id))
            .and_then(|terminal_id| self.state.terminals.get(terminal_id))
        else {
            return Err(("pane_not_found", "pane not found".into()));
        };
        match terminal.state {
            crate::detect::AgentState::Idle => {}
            crate::detect::AgentState::Working => {
                return Err(("agent_working", "the agent is working".into()))
            }
            crate::detect::AgentState::Blocked => {
                return Err(("agent_blocked", "the agent shows a dialog".into()))
            }
            crate::detect::AgentState::Unknown => {
                return Err(("agent_not_ready", "the agent is not ready".into()))
            }
        }
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return Err(("pane_not_found", "pane not found".into()));
        };
        match claude_prompt_line(&runtime.visible_text()) {
            None => {
                return Err((
                    "prompt_not_visible",
                    "the agent's prompt is not on screen".into(),
                ))
            }
            Some(typed) if !typed.is_empty() => {
                return Err(("prompt_has_text", "the agent's prompt holds a draft".into()))
            }
            Some(_) => {}
        }
        let (text, enter) = super::super::api_helpers::encode_api_submission_parts(runtime, text);
        runtime
            .queue_user_input_submission(
                Bytes::from(text),
                Bytes::from(enter),
                Duration::from_millis(300),
                None,
            )
            .map(|_| ())
            .map_err(|err| ("agent_send_failed", err.to_string()))
    }

    fn press_enter_if_prompt_holds(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        text: &str,
    ) {
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return;
        };
        if claude_prompt_line(&runtime.visible_text()).as_deref() != Some(text) {
            return;
        }
        let enter = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        );
        let _ = runtime.try_send_bytes(Bytes::from(runtime.encode_terminal_key(enter.into())));
    }

    fn exit_claude(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        then: AfterAgentExit,
    ) -> Result<(), (&'static str, String)> {
        self.submit_to_waiting_claude(ws_idx, pane_id, "/exit")?;
        if let Some(terminal) = self.terminal_of_pane_mut(ws_idx, pane_id) {
            terminal.pending_agent_exit = Some(PendingAgentExit {
                then,
                requested_at: Instant::now(),
                exited_at: None,
                retried: false,
            });
        }
        self.schedule_agent_insight_refresh();
        Ok(())
    }

    fn claude_resume_argv(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<Vec<String>> {
        let terminal = self
            .state
            .workspaces
            .get(ws_idx)?
            .terminal_id(pane_id)
            .and_then(|terminal_id| self.state.terminals.get(terminal_id))?;
        let argv = terminal.reported_resume()?.argv.clone();
        crate::agent_insight::login_of_argv(&argv)?;
        Some(argv)
    }

    fn claude_login_known(&self, login: &str) -> bool {
        self.claude_login_statuses()
            .iter()
            .any(|status| status.login == login)
    }

    /// What resumes the agent in a pane, under the title the person knows it by.
    fn closed_agent_info(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        argv: Vec<String>,
    ) -> Option<ClosedAgentInfo> {
        let workspace = self.state.workspaces.get(ws_idx)?;
        let tab = workspace
            .tabs
            .iter()
            .find(|tab| tab.panes.contains_key(&pane_id))?;
        let terminal = tab
            .terminal_id(pane_id)
            .and_then(|terminal_id| self.state.terminals.get(terminal_id))?;
        let title = tab
            .custom_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .or_else(|| {
                terminal
                    .insight
                    .transcript_path
                    .as_deref()
                    .and_then(crate::agent_insight::transcript_title)
            })
            .or_else(|| {
                terminal
                    .claude_session_id()
                    .and_then(|id| transcript_title_of(&id))
            })
            .unwrap_or_default();
        Some(ClosedAgentInfo {
            pane_id: self.public_pane_id(ws_idx, pane_id)?,
            workspace_id: self.public_workspace_id(ws_idx),
            title,
            cwd: terminal.cwd.display().to_string(),
            argv,
            closed_at: crate::pane_todo::unix_now(),
        })
    }
}

fn transcript_title_of(session_id: &str) -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let path = crate::agent_insight::find_transcript(std::path::Path::new(&home), session_id)?;
    crate::agent_insight::transcript_title(&path)
}

/// What is typed in Claude's prompt box (the line starting with ❯ in the last
/// lines of the screen), or None when no prompt box shows.
fn claude_prompt_line(screen: &str) -> Option<String> {
    let lines = screen.lines().collect::<Vec<_>>();
    lines
        .iter()
        .rev()
        .take(14)
        .find_map(|line| line.trim_start().strip_prefix('❯'))
        .map(|typed| typed.trim().to_owned())
}

fn not_found(id: String, pane_id: &str) -> String {
    encode_error(id, "pane_not_found", format!("pane {pane_id} not found"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_prompt_box() {
        let screen = "● Готово.\n\n───────\n❯ \n───────\n  5h 10% · reset 15:10\n  ⏵⏵ bypass permissions on\n";
        assert_eq!(claude_prompt_line(screen).as_deref(), Some(""));
        let draft = screen.replace("❯ \n", "❯ черновик\n");
        assert_eq!(claude_prompt_line(&draft).as_deref(), Some("черновик"));
        assert_eq!(claude_prompt_line("just a shell $"), None);
    }
}
