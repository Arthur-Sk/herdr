//! Pane TODO marks and their reminders. The server keeps the earliest reminder
//! time, wakes for it, clears each due reminder (the TODO mark itself stays until
//! a person marks it done) and hands back what to notify about.

use std::time::{Duration, Instant};

use super::App;

/// A reminder that came due, ready to show as a desktop notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DueTodoReminder {
    pub(crate) title: String,
    pub(crate) body: String,
}

impl App {
    pub(crate) fn sync_todo_reminder_due(&mut self) {
        self.todo_reminder_due = self
            .state
            .terminals
            .values()
            .filter_map(|terminal| terminal.todo.and_then(|todo| todo.remind_at))
            .min();
    }

    /// When the server loop must wake for the next reminder. Wall-clock time is
    /// rechecked on every wake, so a suspend that stalls the monotonic clock only
    /// delays the reminder until the next wake after resume.
    pub(crate) fn todo_reminder_deadline(&self, now: Instant) -> Option<Instant> {
        let due = self.todo_reminder_due?;
        let wait = due.saturating_sub(crate::pane_todo::unix_now());
        Some(now + Duration::from_secs(wait))
    }

    /// Clears every reminder due at `now_unix` (Unix seconds) and returns what
    /// to notify about. TODO marks stay.
    pub(crate) fn take_due_todo_reminders(&mut self, now_unix: u64) -> Vec<DueTodoReminder> {
        if self.todo_reminder_due.is_none_or(|due| due > now_unix) {
            return Vec::new();
        }
        let mut due_terminals = Vec::new();
        for terminal in self.state.terminals.values_mut() {
            let Some(todo) = terminal.todo.as_mut() else {
                continue;
            };
            if todo.remind_at.is_some_and(|at| at <= now_unix) {
                todo.remind_at = None;
                due_terminals.push(terminal.id.clone());
            }
        }
        self.sync_todo_reminder_due();
        if due_terminals.is_empty() {
            return Vec::new();
        }
        self.state.mark_session_dirty();

        let mut panes = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for pane_id in tab.panes.keys() {
                    if tab
                        .terminal_id(*pane_id)
                        .is_some_and(|terminal_id| due_terminals.contains(terminal_id))
                    {
                        panes.push((ws_idx, *pane_id));
                    }
                }
            }
        }
        panes.sort_by_key(|(ws_idx, pane_id)| (*ws_idx, pane_id.raw()));
        panes
            .into_iter()
            .filter_map(|(ws_idx, pane_id)| {
                self.emit_pane_updated(ws_idx, pane_id);
                self.todo_reminder_text(ws_idx, pane_id)
            })
            .collect()
    }

    fn todo_reminder_text(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<DueTodoReminder> {
        let workspace = self.state.workspaces.get(ws_idx)?;
        let pane = self.pane_info(ws_idx, pane_id)?;
        let subject = pane
            .title
            .or(pane.label)
            .or(pane.display_agent)
            .or(pane.agent)
            .unwrap_or_else(|| "pane".to_owned());
        let workspace_label =
            workspace.display_name_from(&self.state.terminals, &self.terminal_runtimes);
        Some(DueTodoReminder {
            title: format!("Reminder: {subject}"),
            body: crate::app::actions::notification_context(
                workspace,
                &workspace_label,
                ws_idx,
                pane_id,
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{Method, PaneTodoSetParams, Request};
    use crate::config::Config;
    use crate::workspace::Workspace;

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("herd")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app
    }

    fn set(app: &mut App, pane_id: &str, todo: bool, remind_at: Option<u64>) -> String {
        app.handle_api_request_after_internal_events_drained(Request {
            id: "todo".into(),
            method: Method::PaneTodoSet(PaneTodoSetParams {
                pane_id: pane_id.into(),
                todo,
                remind_at,
            }),
        })
    }

    fn first_pane(app: &App) -> (String, crate::terminal::TerminalId) {
        let workspace = &app.state.workspaces[0];
        let pane_id = workspace.tabs[0].layout.focused();
        (
            app.public_pane_id(0, pane_id).unwrap(),
            workspace.terminal_id(pane_id).unwrap().clone(),
        )
    }

    #[test]
    fn marks_reminds_and_clears_a_pane() {
        let mut app = test_app();
        let (pane, terminal_id) = first_pane(&app);

        let response = set(&mut app, &pane, true, None);
        assert!(response.contains("\"type\":\"pane_info\""), "{response}");
        assert_eq!(
            app.state.terminals[&terminal_id].todo,
            Some(crate::terminal::TerminalTodo { remind_at: None })
        );
        assert_eq!(app.todo_reminder_due, None);

        set(&mut app, &pane, true, Some(1_000));
        assert_eq!(app.todo_reminder_due, Some(1_000));
        assert!(app.take_due_todo_reminders(999).is_empty());

        let due = app.take_due_todo_reminders(1_000);
        assert_eq!(due.len(), 1);
        assert!(due[0].title.starts_with("Reminder: "), "{due:?}");
        // The reminder fires once; the TODO mark stays until marked done.
        assert_eq!(
            app.state.terminals[&terminal_id].todo,
            Some(crate::terminal::TerminalTodo { remind_at: None })
        );
        assert_eq!(app.todo_reminder_due, None);
        assert!(app.take_due_todo_reminders(2_000).is_empty());

        set(&mut app, &pane, true, Some(5_000));
        set(&mut app, &pane, false, Some(6_000));
        assert_eq!(app.state.terminals[&terminal_id].todo, None);
        assert_eq!(app.todo_reminder_due, None);
    }

    #[test]
    fn unknown_pane_is_not_found() {
        let mut app = test_app();
        let response = set(&mut app, "w9:p9", true, None);
        assert!(response.contains("pane_not_found"), "{response}");
    }
}
