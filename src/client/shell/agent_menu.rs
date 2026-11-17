//! What the sidebar's menus do with Claude agents (see `crate::agent_insight`
//! on the server): dismiss a question, relaunch under another login, continue
//! after a limit resets, compact, finish an agent and keep it in the closed
//! list, open a closed one again, and step back a layout change.

use super::agent_insight_view::{self as view, AgentInsightView};
use super::folders::{AgentGroup, GroupHead};
use super::*;
use crate::api::schema::{
    AgentReopenParams, AgentSweepParams, Method, PaneAgentActParams, PaneAgentAction,
};

/// How many layouts undo keeps.
const LAYOUT_UNDO_KEPT: usize = 30;

impl ClientShellState {
    pub(super) fn endpoint_offers(&self, method: &str) -> bool {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .and_then(|endpoint| endpoint.methods.as_ref())
            .is_some_and(|methods| methods.iter().any(|offered| offered == method))
    }

    /// What an agent's menu offers for it: none unless it is a Claude agent
    /// the server reads and acts on.
    pub(super) fn agent_insight_menu(&self, pane_id: &str) -> Option<AgentInsightMenu> {
        if !self.endpoint_offers("pane.agent.act") {
            return None;
        }
        let snapshot = self.snapshot.as_deref()?;
        let agent = snapshot
            .agents
            .iter()
            .find(|agent| agent.pane_id == pane_id)?;
        let insight = AgentInsightView::of(agent);
        if !insight.known {
            return None;
        }
        let logins = view::login_statuses(&snapshot.agents);
        Some(AgentInsightMenu {
            asks: insight.asks,
            limited: insight.limit.is_some(),
            continue_after_reset: insight.continue_after_reset,
            compact: insight.context.is_some()
                && agent.agent_status != crate::api::schema::AgentStatus::Working,
            relaunch: insight.resume_argv.is_some()
                && logins
                    .iter()
                    .any(|status| Some(status.login.as_str()) != insight.login.as_deref()),
        })
    }

    /// The agents under a priority group's head, in any order.
    fn agents_under(&self, head: GroupHead) -> Vec<String> {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return Vec::new();
        };
        snapshot
            .agents
            .iter()
            .filter(|agent| {
                AgentGroup::of_agent(agent, false) == head.group
                    && head
                        .state
                        .is_none_or(|state| AgentGroup::of_agent(agent, true) == state)
            })
            .map(|agent| agent.pane_id.clone())
            .collect()
    }

    /// The menu of a priority group's head: what can be done to all its
    /// agents at once. False when there is nothing, so the click falls through.
    pub(super) fn open_agent_group_context_menu(
        &mut self,
        head: GroupHead,
        x: u16,
        y: u16,
    ) -> bool {
        let shown = head.state.unwrap_or(head.group);
        if !matches!(shown, AgentGroup::Question | AgentGroup::Limit)
            || !self.endpoint_offers("pane.agent.act")
        {
            return false;
        }
        let pane_ids = self.agents_under(head);
        if pane_ids.is_empty() {
            return false;
        }
        let relaunch = pane_ids.iter().any(|pane_id| {
            self.agent_insight_menu(pane_id)
                .is_some_and(|menu| menu.relaunch)
        });
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::AgentGroup {
                head,
                pane_ids,
                relaunch,
            },
            x,
            y,
            highlighted: 0,
        }));
        true
    }

    pub(super) fn open_closed_agent_context_menu(&mut self, key: String, x: u16, y: u16) {
        let relaunch = self.endpoint_offers("agent.reopen")
            && self
                .snapshot
                .as_deref()
                .is_some_and(|snapshot| view::login_statuses(&snapshot.agents).len() > 1);
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::ClosedAgent { key, relaunch },
            x,
            y,
            highlighted: 0,
        }));
    }

    /// The logins to pick from, free ones first, each with its limits.
    fn open_login_menu(&mut self, purpose: LoginPurpose, x: u16, y: u16) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let statuses = view::login_statuses(&snapshot.agents);
        // A single agent is not offered the login it already runs under.
        let current = match &purpose {
            LoginPurpose::Relaunch { pane_ids } if pane_ids.len() == 1 => snapshot
                .agents
                .iter()
                .find(|agent| agent.pane_id == pane_ids[0])
                .and_then(|agent| AgentInsightView::of(agent).login),
            _ => None,
        };
        let choices = view::relaunch_choices(&statuses, current.as_deref());
        if choices.is_empty() {
            self.set_endpoint_error("no other Claude login found");
            return;
        }
        let labels = choices
            .iter()
            .map(|status| view::login_menu_label(status, None))
            .collect();
        let logins = choices.into_iter().map(|status| status.login).collect();
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Logins {
                purpose,
                logins,
                labels,
            },
            x,
            y,
            highlighted: 0,
        }));
    }

    fn act_on_agent(
        &mut self,
        pane_id: String,
        action: PaneAgentAction,
        login: Option<String>,
        text: Option<String>,
        on: Option<bool>,
        outcome: &mut ClientShellInput,
    ) {
        let kind = if action == PaneAgentAction::Finish {
            let folder = self.agent_folder_of(&pane_id);
            PendingEndpointKind::CloseAgents {
                folders: vec![(pane_id.clone(), folder)],
            }
        } else {
            PendingEndpointKind::Generic
        };
        self.push_endpoint_method_with_kind(
            Method::PaneAgentAct(PaneAgentActParams {
                pane_id,
                action,
                login,
                text,
                on,
            }),
            kind,
            outcome,
        );
    }

    fn agent_folder_of(&self, pane_id: &str) -> Option<String> {
        let snapshot = self.snapshot.as_deref()?;
        let agent = snapshot
            .agents
            .iter()
            .find(|agent| agent.pane_id == pane_id)?;
        self.folders
            .agent_folder(&agent.pane_id, &agent.workspace_id)
            .map(str::to_owned)
    }

    /// Runs an agent-menu action on the agents it names. `x`/`y` place a menu
    /// it opens next.
    pub(super) fn activate_agent_action(
        &mut self,
        pane_ids: Vec<String>,
        action: ClientContextMenuAction,
        x: u16,
        y: u16,
        outcome: &mut ClientShellInput,
    ) {
        match action {
            ClientContextMenuAction::DismissQuestion => {
                for pane_id in pane_ids {
                    self.act_on_agent(
                        pane_id,
                        PaneAgentAction::DismissQuestion,
                        None,
                        None,
                        None,
                        outcome,
                    );
                }
            }
            ClientContextMenuAction::ContinueAfterReset
            | ClientContextMenuAction::StopContinueAfterReset => {
                let on = action == ClientContextMenuAction::ContinueAfterReset;
                for pane_id in pane_ids {
                    self.act_on_agent(
                        pane_id,
                        PaneAgentAction::ContinueAfterReset,
                        None,
                        None,
                        Some(on),
                        outcome,
                    );
                }
            }
            ClientContextMenuAction::Compact => {
                for pane_id in pane_ids {
                    self.act_on_agent(
                        pane_id,
                        PaneAgentAction::Send,
                        None,
                        Some("/compact".to_owned()),
                        None,
                        outcome,
                    );
                }
            }
            ClientContextMenuAction::FinishAgent => {
                for pane_id in pane_ids {
                    self.act_on_agent(pane_id, PaneAgentAction::Finish, None, None, None, outcome);
                }
            }
            ClientContextMenuAction::RelaunchAs => {
                self.open_login_menu(LoginPurpose::Relaunch { pane_ids }, x, y);
            }
            _ => {}
        }
    }

    pub(super) fn activate_login_choice(
        &mut self,
        purpose: LoginPurpose,
        login: String,
        outcome: &mut ClientShellInput,
    ) {
        match purpose {
            LoginPurpose::Relaunch { pane_ids } => {
                for pane_id in pane_ids {
                    self.act_on_agent(
                        pane_id,
                        PaneAgentAction::Relaunch,
                        Some(login.clone()),
                        None,
                        None,
                        outcome,
                    );
                }
            }
            LoginPurpose::Reopen { key } => self.reopen_closed_agent(&key, Some(&login), outcome),
        }
    }

    /// Opens a closed agent again in a new tab, under `login` when given.
    pub(super) fn reopen_closed_agent(
        &mut self,
        key: &str,
        login: Option<&str>,
        outcome: &mut ClientShellInput,
    ) {
        let Some(closed) = self.folders.closed.get(key).cloned() else {
            return;
        };
        let mut argv = closed.argv;
        if let (Some(login), Some(program)) = (login, argv.first_mut()) {
            *program = login.to_owned();
        }
        let workspace_id = self.snapshot.as_deref().and_then(|snapshot| {
            snapshot
                .workspaces
                .iter()
                .any(|workspace| workspace.workspace_id == closed.workspace_id)
                .then(|| closed.workspace_id.clone())
        });
        self.push_endpoint_method_with_kind(
            Method::AgentReopen(AgentReopenParams {
                workspace_id,
                cwd: closed.cwd,
                argv,
                label: Some(closed.title).filter(|title| !title.trim().is_empty()),
            }),
            PendingEndpointKind::ReopenAgent {
                key: key.to_owned(),
            },
            outcome,
        );
    }

    pub(super) fn activate_closed_agent_action(
        &mut self,
        key: String,
        action: ClientContextMenuAction,
        x: u16,
        y: u16,
        outcome: &mut ClientShellInput,
    ) {
        match action {
            ClientContextMenuAction::Reopen => self.reopen_closed_agent(&key, None, outcome),
            ClientContextMenuAction::ReopenAs => {
                self.open_login_menu(LoginPurpose::Reopen { key }, x, y)
            }
            ClientContextMenuAction::Forget => self.forget_closed_agent(&key, outcome),
            _ => {}
        }
    }

    /// The agents-section actions that are not about folders. True when
    /// `action` was one of them.
    pub(super) fn activate_agents_section_action(
        &mut self,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) -> bool {
        match action {
            ClientContextMenuAction::CloseFinishedTabs => {
                self.push_endpoint_method_with_kind(
                    Method::AgentSweep(AgentSweepParams { dry_run: false }),
                    PendingEndpointKind::CloseAgents {
                        folders: Vec::new(),
                    },
                    outcome,
                );
            }
            ClientContextMenuAction::ForgetAllClosed => {
                self.folders.closed.clear();
                self.persist_chrome_preferences(outcome);
            }
            ClientContextMenuAction::UndoLayout => self.undo_layout(outcome),
            _ => return false,
        }
        true
    }

    /// Keeps agents the server closed in the closed list, filed where they sat.
    pub(super) fn remember_closed_agents(
        &mut self,
        agents: Vec<crate::api::schema::ClosedAgentInfo>,
        folders: &[(String, Option<String>)],
        outcome: &mut ClientShellInput,
    ) {
        if agents.is_empty() {
            self.set_endpoint_error("no finished Claude tabs to close");
            outcome.repaint = true;
            return;
        }
        for agent in agents {
            let folder = folders
                .iter()
                .find(|(pane_id, _)| *pane_id == agent.pane_id)
                .and_then(|(_, folder)| folder.clone())
                .or_else(|| {
                    self.folders
                        .space_folder(&agent.workspace_id)
                        .map(str::to_owned)
                });
            self.folders.remember_closed(agent, folder);
        }
        self.persist_chrome_preferences(outcome);
    }

    pub(super) fn forget_closed_agent(&mut self, key: &str, outcome: &mut ClientShellInput) {
        if self.folders.closed.remove(key).is_some() {
            self.persist_chrome_preferences(outcome);
        }
    }

    /// Before a save: the layout as it was, when its structure changed since
    /// the last save, goes on the undo stack.
    pub(super) fn note_layout_undo(&mut self) {
        let current = self.folders.clone();
        let Some(mark) = self.layout_undo_mark.replace(current) else {
            return;
        };
        if self
            .layout_undo_mark
            .as_ref()
            .is_some_and(|current| !current.same_structure(&mark))
        {
            self.layout_undo.push(mark);
            if self.layout_undo.len() > LAYOUT_UNDO_KEPT {
                self.layout_undo.remove(0);
            }
        }
    }

    pub(super) fn undo_layout(&mut self, outcome: &mut ClientShellInput) {
        let Some(previous) = self.layout_undo.pop() else {
            self.set_endpoint_error("nothing to undo");
            outcome.repaint = true;
            return;
        };
        self.folders.restore_structure(previous);
        self.layout_undo_mark = Some(self.folders.clone());
        self.store_chrome_preferences(outcome);
        outcome.repaint = true;
    }
}

/// What a Claude agent's menu adds.
pub(super) fn insight_items(menu: &AgentInsightMenu) -> Vec<ClientContextMenuItem> {
    use ClientContextMenuAction as Action;

    let item = |label: &'static str, action| ClientContextMenuItem {
        label: label.into(),
        action,
    };
    let mut items = Vec::new();
    if menu.asks {
        items.push(item("Dismiss question", Action::DismissQuestion));
    }
    if menu.limited {
        items.push(if menu.continue_after_reset {
            item("Don't continue after reset", Action::StopContinueAfterReset)
        } else {
            item("Continue after reset", Action::ContinueAfterReset)
        });
    }
    if menu.relaunch {
        items.push(item("Relaunch as...", Action::RelaunchAs));
    }
    if menu.compact {
        items.push(item("Compact context", Action::Compact));
    }
    items.push(item("Finish agent", Action::FinishAgent));
    items
}

/// What a priority group's head offers for all its agents.
pub(super) fn group_items(head: GroupHead, relaunch: bool) -> Vec<ClientContextMenuItem> {
    use ClientContextMenuAction as Action;

    let item = |label: &'static str, action| ClientContextMenuItem {
        label: label.into(),
        action,
    };
    match head.state.unwrap_or(head.group) {
        AgentGroup::Limit => {
            let mut items = vec![item("Continue all after reset", Action::ContinueAfterReset)];
            if relaunch {
                items.push(item("Relaunch all as...", Action::RelaunchAs));
            }
            items
        }
        AgentGroup::Question => vec![item("Dismiss all questions", Action::DismissQuestion)],
        _ => Vec::new(),
    }
}

pub(super) fn closed_items(relaunch: bool) -> Vec<ClientContextMenuItem> {
    use ClientContextMenuAction as Action;

    let item = |label: &'static str, action| ClientContextMenuItem {
        label: label.into(),
        action,
    };
    let mut items = vec![item("Reopen", Action::Reopen)];
    if relaunch {
        items.push(item("Reopen as...", Action::ReopenAs));
    }
    items.push(item("Forget", Action::Forget));
    items
}

/// Waiting Claude agents in `folder` whose context is over 250k: what
/// "Compact agents over 250k" sends /compact to.
pub(super) fn compactable_in_folder(
    snapshot: &ClientShellSnapshot,
    folders: &super::folders::SidebarFolders,
    folder: &str,
) -> Vec<String> {
    snapshot
        .agents
        .iter()
        .filter(|agent| {
            agent.agent_status != crate::api::schema::AgentStatus::Working
                && folders
                    .agent_folder(&agent.pane_id, &agent.workspace_id)
                    .is_some_and(|inside| super::folders::folder_contains(folder, inside))
                && AgentInsightView::of(agent).compactable()
        })
        .map(|agent| agent.pane_id.clone())
        .collect()
}
