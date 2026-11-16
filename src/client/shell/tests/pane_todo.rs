use super::*;

fn agent(tokens: Vec<(String, String)>) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: Some("claude".into()),
        display_agent: None,
        agent: Some("claude".into()),
        title: Some("Fix the parser".into()),
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Idle,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens,
        focused: false,
    }
}

fn state_with(tokens: Vec<(String, String)>, todo_supported: bool) -> ClientShellState {
    let mut projected = snapshot();
    projected.agents.push(agent(tokens));
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    let mut methods = vec!["pane.rename".to_owned(), "pane.focus".to_owned()];
    if todo_supported {
        methods.push("pane.todo.set".to_owned());
    }
    state.set_endpoint_methods(Some(methods));
    state
}

fn right_click_agent_row(state: &mut ClientShellState) -> ClientShellInput {
    state.compose(106, 30).expect("agent sidebar frame");
    let row = state.hits.agents[0].0;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: row.x + 2,
        row: row.y,
        modifiers: KeyModifiers::empty(),
    })])
}

fn menu_actions(state: &ClientShellState) -> Vec<ClientContextMenuAction> {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::ContextMenu(menu)) => {
            menu.items().iter().map(|item| item.action).collect()
        }
        _ => Vec::new(),
    }
}

fn choose(state: &mut ClientShellState, action: ClientContextMenuAction) -> ClientShellInput {
    let index = menu_actions(state)
        .iter()
        .position(|candidate| *candidate == action)
        .expect("menu item");
    state.compose(106, 30).expect("context menu frame");
    let row = state.hits.context_menu_rows[index].0;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: row.x + 1,
        row: row.y,
        modifiers: KeyModifiers::empty(),
    })])
}

fn todo_set(outcome: &ClientShellInput) -> crate::api::schema::PaneTodoSetParams {
    let [ClientShellAction::Endpoint { request, .. }] = &outcome.actions[..] else {
        panic!("expected one endpoint request, got {:?}", outcome.actions);
    };
    let crate::api::schema::Method::PaneTodoSet(params) = &request.method else {
        panic!("expected pane.todo.set, got {:?}", request.method);
    };
    params.clone()
}

fn todo_tokens(remind_at: Option<u64>) -> Vec<(String, String)> {
    let mut tokens = vec![(crate::pane_todo::TODO_TOKEN.to_owned(), "1".to_owned())];
    if let Some(at) = remind_at {
        tokens.push((crate::pane_todo::REMIND_AT_TOKEN.to_owned(), at.to_string()));
    }
    tokens
}

fn screen_text(frame: &crate::client::frame_output::ComposedFrame) -> String {
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

#[test]
fn agent_row_menu_marks_todo_through_the_endpoint_api() {
    let mut state = state_with(Vec::new(), true);
    let open = right_click_agent_row(&mut state);
    assert!(open.actions.is_empty());
    assert_eq!(
        menu_actions(&state),
        vec![
            ClientContextMenuAction::MarkTodo,
            ClientContextMenuAction::RemindMe,
            ClientContextMenuAction::MoveToFolder,
        ]
    );

    let params = todo_set(&choose(&mut state, ClientContextMenuAction::MarkTodo));
    assert_eq!(params.pane_id, "pane_1");
    assert!(params.todo);
    assert_eq!(params.remind_at, None);
}

#[test]
fn marked_agent_offers_done_and_reminder_changes() {
    let mut state = state_with(todo_tokens(Some(4_000_000_000)), true);
    right_click_agent_row(&mut state);
    assert_eq!(
        menu_actions(&state),
        vec![
            ClientContextMenuAction::MarkTodoDone,
            ClientContextMenuAction::RemindMe,
            ClientContextMenuAction::CancelReminder,
            ClientContextMenuAction::MoveToFolder,
        ]
    );
    let params = todo_set(&choose(&mut state, ClientContextMenuAction::MarkTodoDone));
    assert!(!params.todo);

    right_click_agent_row(&mut state);
    let params = todo_set(&choose(&mut state, ClientContextMenuAction::CancelReminder));
    assert!(params.todo);
    assert_eq!(params.remind_at, None);
}

#[test]
fn agent_row_menu_offers_only_folders_when_the_server_lacks_todo_marks() {
    let mut state = state_with(Vec::new(), false);
    let open = right_click_agent_row(&mut state);
    assert!(open.actions.is_empty());
    assert_eq!(
        menu_actions(&state),
        vec![ClientContextMenuAction::MoveToFolder]
    );
}

#[test]
fn pane_menu_offers_todo_items_for_agent_panes() {
    let mut state = state_with(Vec::new(), true);
    state.compose(106, 30).expect("composed frame");
    let pane = state.hits.panes[0].rect;
    state.handle_raw_events(vec![RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: pane.x + 1,
        row: pane.y,
        modifiers: KeyModifiers::empty(),
    })]);
    let actions = menu_actions(&state);
    assert!(actions.contains(&ClientContextMenuAction::RenamePane));
    assert!(actions.contains(&ClientContextMenuAction::MarkTodo));
    assert!(actions.contains(&ClientContextMenuAction::RemindMe));
}

#[test]
fn remind_me_reads_the_typed_time_and_keeps_the_dialog_on_bad_input() {
    let mut state = state_with(Vec::new(), true);
    right_click_agent_row(&mut state);
    let open = choose(&mut state, ClientContextMenuAction::RemindMe);
    assert!(open.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::Reminder { ref pane_id },
            ..
        })) if pane_id == "pane_1"
    ));

    // The prefilled "1h" is replaced by what is typed.
    assert!(state.handle_input_bytes(b"soon").actions.is_empty());
    assert!(state.handle_input_bytes(b"\r").actions.is_empty());
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::Rename(rename)) => {
            assert!(rename.title.starts_with("can't read that"));
        }
        _ => panic!("reminder dialog should stay open"),
    }

    state.handle_input_bytes(&[0x03]);
    state.handle_input_bytes(b"2h");
    let before = crate::pane_todo::unix_now();
    let params = todo_set(&state.handle_input_bytes(b"\r"));
    assert!(params.todo);
    let remind_at = params.remind_at.expect("reminder");
    assert!((before + 7200..=crate::pane_todo::unix_now() + 7200).contains(&remind_at));
    assert!(state.overlay.is_none());
}

#[test]
fn todo_rows_carry_a_mark_and_their_reminder_time() {
    let now = crate::pane_todo::unix_now();
    let remind_at = now + 3600;
    let mut state = state_with(todo_tokens(Some(remind_at)), true);
    let frame = state.compose(106, 30).expect("agent sidebar frame");
    let text = screen_text(&frame);
    let expected = crate::pane_todo::format_remind_at(
        remind_at,
        now,
        crate::pane_todo::cached_local_utc_offset_secs(),
    );
    assert!(text.contains('▌'), "{text}");
    assert!(text.contains(&format!("◷ {expected}")), "{text}");

    let mut plain = state_with(Vec::new(), true);
    let frame = plain.compose(106, 30).expect("agent sidebar frame");
    let text = screen_text(&frame);
    assert!(!text.contains('▌'), "{text}");
    assert!(!text.contains('◷'), "{text}");
}
