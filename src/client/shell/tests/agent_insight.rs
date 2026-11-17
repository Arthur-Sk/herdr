use super::*;
use crate::agent_insight as insight;
use crate::api::schema::{Method, PaneAgentAction};

const LOGINS: &str = r#"[{"login":"claude","agents":1,"five_hour_percent":91},{"login":"claude-cc","agents":0,"five_hour_percent":12}]"#;

fn token(key: &str, value: &str) -> (String, String) {
    (key.to_owned(), value.to_owned())
}

/// A Claude agent the server reads: in `pane_<n>`, with `tokens` on top of
/// the logins every such agent carries.
fn claude(n: usize, status: AgentStatus, tokens: &[(&str, &str)]) -> ClientShellAgent {
    let mut all = vec![token(insight::LOGINS_TOKEN, LOGINS)];
    all.extend(tokens.iter().map(|(key, value)| token(key, value)));
    ClientShellAgent {
        pane_id: format!("pane_{n}"),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: Some("claude".into()),
        display_agent: None,
        agent: Some("claude".into()),
        title: Some(format!("agent {n}")),
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: status,
        state_change_seq: n as u64,
        state_labels: Vec::new(),
        tokens: all,
        focused: false,
    }
}

fn state_with(agents: Vec<ClientShellAgent>) -> ClientShellState {
    let mut projected = snapshot();
    projected.agents = agents;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.set_endpoint_methods(Some(vec![
        "pane.focus".to_owned(),
        "pane.agent.act".to_owned(),
        "agent.sweep".to_owned(),
        "agent.reopen".to_owned(),
    ]));
    state
}

fn screen(state: &mut ClientShellState) -> String {
    let frame = state.compose(106, 60).expect("sidebar frame");
    frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn mouse(
    state: &mut ClientShellState,
    kind: MouseEventKind,
    (column, row): (u16, u16),
) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::empty(),
    })])
}

fn agent_at(state: &ClientShellState, pane_id: &str) -> (u16, u16) {
    let (rect, _) = state
        .hits
        .agents
        .iter()
        .find(|(_, candidate)| candidate == pane_id)
        .unwrap_or_else(|| panic!("agent {pane_id}"));
    (rect.x + 4, rect.y)
}

fn shown_agents(state: &ClientShellState) -> Vec<String> {
    state
        .hits
        .agents
        .iter()
        .map(|(_, pane_id)| pane_id.clone())
        .collect()
}

fn menu_actions(state: &ClientShellState) -> Vec<ClientContextMenuAction> {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => {
            menu.items().iter().map(|item| item.action).collect()
        }
        _ => Vec::new(),
    }
}

fn menu_labels(state: &ClientShellState) -> Vec<String> {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => menu
            .items()
            .iter()
            .map(|item| item.label.to_string())
            .collect(),
        _ => Vec::new(),
    }
}

fn choose(state: &mut ClientShellState, action: ClientContextMenuAction) -> ClientShellInput {
    let index = menu_actions(state)
        .iter()
        .position(|candidate| *candidate == action)
        .unwrap_or_else(|| panic!("menu item {action:?} in {:?}", menu_actions(state)));
    screen(state);
    let row = state.hits.context_menu_rows[index].0;
    mouse(
        state,
        MouseEventKind::Down(MouseButton::Left),
        (row.x + 1, row.y),
    )
}

fn requests(outcome: &ClientShellInput) -> Vec<(String, Method)> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => {
                Some((request.id.clone(), request.method.clone()))
            }
            _ => None,
        })
        .collect()
}

fn agent_act(outcome: &ClientShellInput) -> (String, crate::api::schema::PaneAgentActParams) {
    match &requests(outcome)[..] {
        [(id, Method::PaneAgentAct(params))] => (id.clone(), params.clone()),
        other => panic!("expected one pane.agent.act, got {other:?}"),
    }
}

fn in_an_hour() -> String {
    (crate::pane_todo::unix_now() + 3600).to_string()
}

#[test]
fn asking_and_limited_agents_get_groups_of_their_own_inside_todo_too() {
    let limit = in_an_hour();
    let mut state = state_with(vec![
        claude(1, AgentStatus::Idle, &[(insight::QUESTION_TOKEN, "1")]),
        claude(2, AgentStatus::Done, &[(insight::LIMIT_TOKEN, &limit)]),
        // A working agent's last message no longer stands.
        claude(3, AgentStatus::Working, &[(insight::QUESTION_TOKEN, "1")]),
        claude(4, AgentStatus::Blocked, &[]),
        claude(
            5,
            AgentStatus::Idle,
            &[
                (crate::pane_todo::TODO_TOKEN, "1"),
                (insight::QUESTION_TOKEN, "1"),
            ],
        ),
        claude(6, AgentStatus::Idle, &[]),
    ]);
    state.config.agent_panel_sort = crate::config::AgentPanelSortConfig::Priority;
    let text = screen(&mut state);
    let line_of = |needle: &str| {
        text.lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle} in\n{text}"))
    };
    let heads = [
        "todo 1",
        "question 1",
        "question 2",
        "limit 1",
        "working 1",
        "idle 1",
    ]
    .map(line_of);
    assert!(heads.is_sorted(), "{heads:?}\n{text}");
    // Blocked before a seen question; the limit's reset time on its row.
    assert_eq!(
        shown_agents(&state),
        ["pane_5", "pane_4", "pane_1", "pane_2", "pane_3", "pane_6"]
    );
    let row_text = |pane_id: &str| {
        let (rect, _) = state
            .hits
            .agents
            .iter()
            .find(|(_, candidate)| candidate == pane_id)
            .expect("agent row");
        text.lines()
            .skip(usize::from(rect.y))
            .take(usize::from(rect.height))
            .map(|line| {
                line.chars()
                    .take(usize::from(rect.right()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let limited = row_text("pane_2");
    assert!(limited.contains("⏸ "), "{limited}");
    let asking = row_text("pane_1");
    assert!(asking.contains('?'), "{asking}");
}

#[test]
fn agent_menu_dismisses_a_question_and_relaunches_under_a_free_login() {
    let argv = r#"["claude","--resume","5b1c"]"#;
    let mut state = state_with(vec![claude(
        1,
        AgentStatus::Idle,
        &[
            (insight::QUESTION_TOKEN, "1"),
            (insight::CONTEXT_TOKEN, "312000"),
            (insight::LOGIN_TOKEN, "claude"),
            (insight::RESUME_TOKEN, argv),
        ],
    )]);
    screen(&mut state);
    let at = agent_at(&state, "pane_1");
    mouse(&mut state, MouseEventKind::Down(MouseButton::Right), at);
    assert_eq!(
        menu_actions(&state),
        [
            ClientContextMenuAction::DismissQuestion,
            ClientContextMenuAction::RelaunchAs,
            ClientContextMenuAction::Compact,
            ClientContextMenuAction::FinishAgent,
            ClientContextMenuAction::MoveToFolder,
        ]
    );
    let (_, params) = agent_act(&choose(
        &mut state,
        ClientContextMenuAction::DismissQuestion,
    ));
    assert_eq!(params.pane_id, "pane_1");
    assert_eq!(params.action, PaneAgentAction::DismissQuestion);

    mouse(&mut state, MouseEventKind::Down(MouseButton::Right), at);
    let open = choose(&mut state, ClientContextMenuAction::RelaunchAs);
    assert!(requests(&open).is_empty());
    // Its own login is not offered; the others say how used they are.
    let labels = menu_labels(&state);
    assert_eq!(labels.len(), 1, "{labels:?}");
    assert!(
        labels[0].starts_with("cc") && labels[0].contains("5h 12%"),
        "{labels:?}"
    );
    let (_, params) = agent_act(&choose(&mut state, ClientContextMenuAction::PickLogin(0)));
    assert_eq!(params.action, PaneAgentAction::Relaunch);
    assert_eq!(params.login.as_deref(), Some("claude-cc"));

    mouse(&mut state, MouseEventKind::Down(MouseButton::Right), at);
    let (_, params) = agent_act(&choose(&mut state, ClientContextMenuAction::Compact));
    assert_eq!(params.action, PaneAgentAction::Send);
    assert_eq!(params.text.as_deref(), Some("/compact"));
}

#[test]
fn the_limit_group_continues_all_its_agents_after_reset() {
    let limit = in_an_hour();
    let mut state = state_with(vec![
        claude(1, AgentStatus::Idle, &[(insight::LIMIT_TOKEN, &limit)]),
        claude(2, AgentStatus::Idle, &[(insight::LIMIT_TOKEN, "login")]),
        claude(3, AgentStatus::Idle, &[]),
    ]);
    state.config.agent_panel_sort = crate::config::AgentPanelSortConfig::Priority;
    screen(&mut state);
    let (head, group) = state.hits.agent_groups[0];
    assert_eq!(group.label(), "limit");
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Right),
        (head.x + 4, head.y),
    );
    let outcome = choose(&mut state, ClientContextMenuAction::ContinueAfterReset);
    let mut continued = requests(&outcome)
        .into_iter()
        .map(|(_, method)| match method {
            Method::PaneAgentAct(params) => {
                assert_eq!(params.action, PaneAgentAction::ContinueAfterReset);
                assert_eq!(params.on, Some(true));
                params.pane_id
            }
            other => panic!("{other:?}"),
        })
        .collect::<Vec<_>>();
    continued.sort();
    assert_eq!(continued, ["pane_1", "pane_2"]);
}

#[test]
fn a_finished_agent_waits_in_the_closed_group_and_a_click_reopens_it() {
    let mut state = state_with(vec![claude(1, AgentStatus::Idle, &[])]);
    screen(&mut state);
    let at = agent_at(&state, "pane_1");
    mouse(&mut state, MouseEventKind::Down(MouseButton::Right), at);
    let (request_id, params) = agent_act(&choose(&mut state, ClientContextMenuAction::FinishAgent));
    assert_eq!(params.action, PaneAgentAction::Finish);
    let closed = crate::api::schema::ClosedAgentInfo {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        title: "Fix the parser".into(),
        cwd: "/repo".into(),
        argv: vec!["claude".into(), "--resume".into(), "5b1c".into()],
        closed_at: crate::pane_todo::unix_now() - 7200,
    };
    state.handle_endpoint_result(
        "boot-1",
        &request_id,
        Ok(crate::api::schema::ResponseResult::ClosedAgents {
            agents: vec![closed],
        }),
    );
    assert!(state.folders.closed.contains_key("5b1c"));

    // Folded at first; a click on its head opens it.
    let text = screen(&mut state);
    assert!(text.contains("▸ × closed 1"), "{text}");
    assert!(state.hits.closed_agents.is_empty());
    let (head, _) = *state
        .hits
        .agent_groups
        .iter()
        .find(|(_, head)| head.label() == "closed")
        .expect("closed head");
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        (head.x + 3, head.y),
    );
    mouse(
        &mut state,
        MouseEventKind::Up(MouseButton::Left),
        (head.x + 3, head.y),
    );
    let text = screen(&mut state);
    assert!(
        text.contains("Fix the parser") && text.contains("2h"),
        "{text}"
    );

    let (row, _) = state.hits.closed_agents[0];
    let outcome = mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Left),
        (row.x + 3, row.y),
    );
    let [(request_id, Method::AgentReopen(params))] = &requests(&outcome)[..] else {
        panic!("expected agent.reopen, got {:?}", outcome.actions);
    };
    assert_eq!(params.workspace_id.as_deref(), Some("ws_1"));
    assert_eq!(params.argv, ["claude", "--resume", "5b1c"]);
    assert_eq!(params.label.as_deref(), Some("Fix the parser"));
    state.handle_endpoint_result(
        "boot-1",
        request_id,
        Ok(crate::api::schema::ResponseResult::Ok {}),
    );
    assert!(state.folders.closed.is_empty());
}

#[test]
fn undo_steps_back_layout_changes_one_at_a_time() {
    let mut state = state_with(vec![claude(1, AgentStatus::Idle, &[])]);
    let mut outcome = ClientShellInput::default();
    state.folders.file_space("ws_1", Some("A"));
    state.persist_chrome_preferences(&mut outcome);
    state.folders.file_space("ws_1", Some("B"));
    state.persist_chrome_preferences(&mut outcome);
    // Folding is not a layout change.
    state
        .folders
        .toggle_collapsed(super::folders::FolderSection::Spaces, "B");
    state.persist_chrome_preferences(&mut outcome);

    state.undo_layout(&mut outcome);
    assert_eq!(state.folders.space_folder("ws_1"), Some("A"));
    assert!(state
        .folders
        .collapsed(super::folders::FolderSection::Spaces)
        .contains("B"));
    state.undo_layout(&mut outcome);
    assert_eq!(state.folders.space_folder("ws_1"), None);
    assert!(state.layout_undo.is_empty());
}

#[test]
fn resting_on_an_agent_row_shows_its_card() {
    let excerpt = "Два варианта.\n\nРекомендация: A. Что выбираешь?";
    let limit = in_an_hour();
    let mut state = state_with(vec![claude(
        1,
        AgentStatus::Idle,
        &[
            (insight::QUESTION_TOKEN, "1"),
            (insight::EXCERPT_TOKEN, excerpt),
            (insight::CONTEXT_TOKEN, "212000"),
            (insight::MODEL_TOKEN, "claude-opus-5-5"),
            (insight::LOGIN_TOKEN, "claude"),
            (insight::LIMIT_TOKEN, &limit),
        ],
    )]);
    screen(&mut state);
    let at = agent_at(&state, "pane_1");
    mouse(&mut state, MouseEventKind::Moved, at);
    assert!(!screen(&mut state).contains("asks you"));
    assert!(state.tick_agent_hover(std::time::Instant::now() + std::time::Duration::from_secs(1)));
    let text = screen(&mut state);
    for expected in [
        "claude · opus 5.5 · ctx 212k · 5h 91%",
        "⏸ limit until",
        "? asks you",
        "Рекомендация: A. Что выбираешь?",
    ] {
        assert!(text.contains(expected), "{expected} in\n{text}");
    }
    // A key or moving off the row hides it.
    mouse(&mut state, MouseEventKind::Moved, (at.0, at.1 + 10));
    assert!(!screen(&mut state).contains("asks you"));
}

#[test]
fn a_second_right_click_opens_the_menu_again() {
    let mut state = state_with(vec![
        claude(1, AgentStatus::Idle, &[]),
        claude(2, AgentStatus::Idle, &[]),
    ]);
    let menu_of = |state: &ClientShellState| match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Agent { pane_id, .. },
            ..
        })) => Some(pane_id.clone()),
        _ => None,
    };
    screen(&mut state);
    let first = agent_at(&state, "pane_1");
    let second = agent_at(&state, "pane_2");
    mouse(&mut state, MouseEventKind::Down(MouseButton::Right), first);
    assert_eq!(menu_of(&state).as_deref(), Some("pane_1"));
    // The menu opened at the pointer: the same spot again lands on it.
    screen(&mut state);
    mouse(&mut state, MouseEventKind::Up(MouseButton::Right), first);
    mouse(&mut state, MouseEventKind::Down(MouseButton::Right), first);
    assert_eq!(menu_of(&state).as_deref(), Some("pane_1"));
    // Another row, even under the open menu, gets its own.
    screen(&mut state);
    mouse(&mut state, MouseEventKind::Down(MouseButton::Right), second);
    assert_eq!(menu_of(&state).as_deref(), Some("pane_2"));
    // Once the menu is closed, a right click opens it as before.
    mouse(&mut state, MouseEventKind::Down(MouseButton::Left), (0, 0));
    screen(&mut state);
    assert_eq!(menu_of(&state), None);
    mouse(&mut state, MouseEventKind::Down(MouseButton::Right), first);
    assert_eq!(menu_of(&state).as_deref(), Some("pane_1"));
}

#[test]
fn the_card_title_skips_the_shell_title_once_the_tab_has_a_name() {
    let mut agent = claude(1, AgentStatus::Idle, &[]);
    agent.title = None;
    agent.terminal_title_stripped = Some("user@host: ~".into());
    let mut projected = snapshot();
    projected.agents = vec![agent];
    projected.tabs[0].label = "daily skill".into();
    projected.tabs[0].custom_label = true;
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    screen(&mut state);
    let at = agent_at(&state, "pane_1");
    mouse(&mut state, MouseEventKind::Moved, at);
    state.tick_agent_hover(std::time::Instant::now() + std::time::Duration::from_secs(1));
    let text = screen(&mut state);
    assert!(text.contains("client-shell · daily skill"), "{text}");
    assert!(!text.contains("user@host"), "{text}");
}

#[test]
fn closed_agents_merge_like_the_rest_of_the_layout() {
    use super::folders::{ClosedAgent, SidebarFolders};

    let closed = |title: &str| ClosedAgent {
        title: title.into(),
        workspace_id: "ws_1".into(),
        cwd: "/repo".into(),
        argv: vec!["claude".into()],
        closed_at: 1,
        folder: None,
    };
    let mut base = SidebarFolders::default();
    base.closed.insert("a".into(), closed("a"));
    let mut ours = base.clone();
    ours.closed.remove("a");
    let mut theirs = base.clone();
    theirs.closed.insert("b".into(), closed("b"));
    let merged = SidebarFolders::merged(&base, ours, theirs);
    assert_eq!(merged.closed.keys().collect::<Vec<_>>(), ["b"]);
}
