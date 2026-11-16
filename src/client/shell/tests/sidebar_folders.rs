use super::*;
use crate::client::shell::folders::{FolderSection, SidebarFolders};

const SIZE: (u16, u16) = (110, 40);

fn workspace(id: &str, label: &str, focused: bool) -> ClientShellWorkspace {
    ClientShellWorkspace {
        workspace_id: id.into(),
        active_tab_id: format!("{id}:t1"),
        new_workspace_cwd: "/repo".into(),
        number: 1,
        label: label.into(),
        custom_label: true,
        branch: None,
        git_ahead_behind: None,
        tokens: Vec::new(),
        worktree: None,
        focused,
        agent_status: AgentStatus::Idle,
    }
}

fn agent(workspace_id: &str, title: &str, focused: bool) -> crate::protocol::ClientShellAgent {
    crate::protocol::ClientShellAgent {
        pane_id: format!("{workspace_id}:p1"),
        workspace_id: workspace_id.into(),
        tab_id: format!("{workspace_id}:t1"),
        name: Some("claude".into()),
        display_agent: None,
        agent: Some("claude".into()),
        title: Some(title.into()),
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Idle,
        state_change_seq: 1,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused,
    }
}

/// Three spaces with one agent each: w1 "client-shell" (focused), w2
/// "thex-core", w3 "home".
fn state_with(folders: SidebarFolders) -> ClientShellState {
    let mut projected = snapshot();
    projected.focused_workspace_id = Some("w1".into());
    projected.focused_tab_id = Some("w1:t1".into());
    projected.focused_pane_id = Some("w1:p1".into());
    projected.workspaces = vec![
        workspace("w1", "client-shell", true),
        workspace("w2", "thex-core", false),
        workspace("w3", "home", false),
    ];
    projected.tabs = ["w1", "w2", "w3"]
        .into_iter()
        .map(|id| ClientShellTab {
            tab_id: format!("{id}:t1"),
            workspace_id: id.into(),
            number: 1,
            label: "1".into(),
            custom_label: false,
            zoomed: false,
            focused: id == "w1",
            agent_status: AgentStatus::Idle,
        })
        .collect();
    projected.panes = ["w1", "w2", "w3"]
        .into_iter()
        .map(|id| ClientShellPane {
            pane_id: format!("{id}:p1"),
            workspace_id: id.into(),
            tab_id: format!("{id}:t1"),
            label: None,
            cwd: Some("/repo".into()),
            foreground_cwd: Some("/repo".into()),
            focused: id == "w1",
            right_click_passthrough: false,
        })
        .collect();
    projected.agents = vec![
        agent("w1", "Fix review notes", true),
        agent("w2", "THEX review", false),
        agent("w3", "battery", false),
    ];
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.folders = folders;
    state.set_snapshot(Box::new(projected));
    let mut pane_surface = surface();
    pane_surface.panes[0].pane_id = "w1:p1".into();
    state.set_pane_surface(pane_surface);
    state.set_endpoint_methods(Some(vec![
        "pane.focus".to_owned(),
        "workspace.move".to_owned(),
        "workspace.focus".to_owned(),
    ]));
    state
}

fn compose(state: &mut ClientShellState) -> String {
    let frame = state.compose(SIZE.0, SIZE.1).expect("sidebar frame");
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

fn drag(state: &mut ClientShellState, from: (u16, u16), to: (u16, u16)) -> ClientShellInput {
    mouse(state, MouseEventKind::Down(MouseButton::Left), from);
    mouse(state, MouseEventKind::Drag(MouseButton::Left), to);
    compose(state);
    mouse(state, MouseEventKind::Up(MouseButton::Left), to)
}

fn click(state: &mut ClientShellState, at: (u16, u16)) -> ClientShellInput {
    mouse(state, MouseEventKind::Down(MouseButton::Left), at);
    mouse(state, MouseEventKind::Up(MouseButton::Left), at)
}

fn folder_at(state: &ClientShellState, section: FolderSection, path: &str) -> (u16, u16) {
    let hit = state
        .hits
        .folders
        .iter()
        .find(|hit| hit.section == section && hit.path == path)
        .unwrap_or_else(|| panic!("folder {path} in {section:?}"));
    (hit.rect.x + 3, hit.rect.y)
}

fn space_at(state: &ClientShellState, workspace_id: &str) -> (u16, u16) {
    let hit = state
        .hits
        .workspaces
        .iter()
        .find(|hit| hit.workspace_id == workspace_id)
        .unwrap_or_else(|| panic!("space {workspace_id}"));
    (hit.rect.x + 4, hit.rect.y)
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

fn shown_spaces(state: &ClientShellState) -> Vec<String> {
    state
        .hits
        .workspaces
        .iter()
        .map(|hit| hit.workspace_id.clone())
        .collect()
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

fn choose(state: &mut ClientShellState, action: ClientContextMenuAction) -> ClientShellInput {
    let index = menu_actions(state)
        .iter()
        .position(|candidate| *candidate == action)
        .unwrap_or_else(|| panic!("menu item {action:?} in {:?}", menu_actions(state)));
    compose(state);
    let row = state.hits.context_menu_rows[index].0;
    click(state, (row.x + 1, row.y))
}

fn filed(folders: &[(&str, &str)]) -> SidebarFolders {
    let mut sidebar = SidebarFolders::default();
    for (workspace_id, folder) in folders {
        sidebar.file_space(workspace_id, Some(folder));
    }
    sidebar
}

#[test]
fn nested_folders_group_spaces_and_their_agents() {
    let mut state = state_with(filed(&[("w2", "ASOUP/THEX")]));
    let text = compose(&mut state);
    let line_of = |needle: &str| {
        text.lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} on screen:\n{text}"))
    };
    let asoup = line_of("▾ ASOUP");
    let thex = line_of("▾ THEX");
    let core = line_of("thex-core");
    assert!(asoup < thex && thex < core, "{text}");
    // Folders come first, then the spaces left at the top level.
    assert_eq!(shown_spaces(&state), ["w2", "w1", "w3"]);
    // The agent follows its space into the folder.
    assert_eq!(shown_agents(&state), ["w2:p1", "w1:p1", "w3:p1"]);
    let agent_folders = state
        .hits
        .folders
        .iter()
        .filter(|hit| hit.section == FolderSection::Agents)
        .map(|hit| hit.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(agent_folders, ["ASOUP", "ASOUP/THEX"]);
    // Rows inside a folder are indented one step per level.
    let asoup_line = text.lines().nth(asoup).unwrap();
    let thex_line = text.lines().nth(thex).unwrap();
    assert_eq!(
        thex_line.find('▾').unwrap(),
        asoup_line.find('▾').unwrap() + 2
    );
}

#[test]
fn clicking_a_folder_collapses_it_but_keeps_the_focused_space() {
    let mut state = state_with(filed(&[("w1", "Work"), ("w3", "Work")]));
    compose(&mut state);
    let work = folder_at(&state, FolderSection::Spaces, "Work");
    click(&mut state, work);
    assert!(state.folders.collapsed_spaces.contains("Work"));
    assert!(!state.folders.collapsed_agents.contains("Work"));
    let text = compose(&mut state);
    assert!(text.contains("▸ Work"), "{text}");
    assert_eq!(shown_spaces(&state), ["w1", "w2"]);
    // Keyboard navigation skips what a collapsed folder hides.
    let snapshot = state.snapshot.clone().unwrap();
    let order = state
        .navigation_workspace_entries(&snapshot)
        .into_iter()
        .map(|entry| snapshot.workspaces[entry.index].workspace_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(order, ["w1", "w2"]);

    let work = folder_at(&state, FolderSection::Spaces, "Work");
    click(&mut state, work);
    compose(&mut state);
    assert_eq!(shown_spaces(&state), ["w1", "w3", "w2"]);
}

#[test]
fn dragging_an_agent_onto_a_folder_files_it_apart_from_its_space() {
    let mut folders = SidebarFolders::default();
    folders.create("Later");
    let mut state = state_with(folders);
    compose(&mut state);
    let from = agent_at(&state, "w3:p1");
    let to = folder_at(&state, FolderSection::Agents, "Later");
    let outcome = drag(&mut state, from, to);
    assert!(outcome.repaint);
    assert_eq!(state.folders.agent_folder("w3:p1", "w3"), Some("Later"));
    assert_eq!(state.folders.space_folder("w3"), None);
    compose(&mut state);
    assert_eq!(shown_agents(&state), ["w3:p1", "w1:p1", "w2:p1"]);

    // Dropped on the agents header it goes back to the top level.
    let from = agent_at(&state, "w3:p1");
    let header = state.hits.agents_header;
    drag(&mut state, from, (header.x + 2, header.y));
    assert!(!state.folders.agent_filed_apart("w3:p1"));
    assert_eq!(state.folders.agent_folder("w3:p1", "w3"), None);
}

fn workspace_moves(outcome: &ClientShellInput) -> Vec<crate::api::schema::WorkspaceMoveParams> {
    outcome
        .actions
        .iter()
        .filter_map(|action| match action {
            ClientShellAction::Endpoint { request, .. } => match &request.method {
                crate::api::schema::Method::WorkspaceMove(params) => Some(params.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn workspace_move(
    workspace_id: &str,
    insert_index: usize,
) -> crate::api::schema::WorkspaceMoveParams {
    crate::api::schema::WorkspaceMoveParams {
        workspace_id: workspace_id.into(),
        insert_index,
    }
}

#[test]
fn dragging_a_space_files_it_into_a_folder_and_beside_a_space() {
    // ASOUP / THEX / w1, w2; then w3 at the top level.
    let mut state = state_with(filed(&[("w1", "ASOUP/THEX"), ("w2", "ASOUP/THEX")]));
    compose(&mut state);
    assert_eq!(shown_spaces(&state), ["w1", "w2", "w3"]);

    // Over a space row the drop line sits under it: before the next space,
    // in that space's folder.
    let from = space_at(&state, "w3");
    let to = space_at(&state, "w1");
    mouse(&mut state, MouseEventKind::Down(MouseButton::Left), from);
    mouse(&mut state, MouseEventKind::Drag(MouseButton::Left), to);
    assert!(
        matches!(
            state.chrome_drag,
            Some(ClientChromeDrag::Workspace {
                target: Some(WorkspaceDropTarget::Slot {
                    before_workspace_id: Some(ref before),
                    folder: Some(FolderDropTarget::Folder(ref folder)),
                    ..
                }),
                ..
            }) if before == "w2" && folder == "ASOUP/THEX"
        ),
        "{:?}",
        state.chrome_drag.as_ref().map(|_| ())
    );
    let text = compose(&mut state);
    assert!(text.contains("──"), "drop line:\n{text}");
    let drop = mouse(&mut state, MouseEventKind::Up(MouseButton::Left), to);
    assert_eq!(state.folders.space_folder("w3"), Some("ASOUP/THEX"));
    assert_eq!(workspace_moves(&drop), [workspace_move("w3", 1)]);

    // A folder row puts the space on top of the folder's own spaces.
    compose(&mut state);
    let from = space_at(&state, "w2");
    let to = folder_at(&state, FolderSection::Spaces, "ASOUP/THEX");
    let drop = drag(&mut state, from, to);
    assert_eq!(state.folders.space_folder("w2"), Some("ASOUP/THEX"));
    assert_eq!(workspace_moves(&drop), [workspace_move("w2", 0)]);

    compose(&mut state);
    let from = space_at(&state, "w2");
    let to = folder_at(&state, FolderSection::Spaces, "ASOUP");
    let drop = drag(&mut state, from, to);
    assert_eq!(state.folders.space_folder("w2"), Some("ASOUP"));
    assert!(workspace_moves(&drop).is_empty());
}

#[test]
fn dragging_a_folder_nests_it_and_the_header_lifts_it_out() {
    let mut folders = filed(&[("w2", "THEX")]);
    folders.create("ASOUP");
    let mut state = state_with(folders);
    compose(&mut state);
    let from = folder_at(&state, FolderSection::Spaces, "THEX");
    let to = folder_at(&state, FolderSection::Spaces, "ASOUP");
    drag(&mut state, from, to);
    assert_eq!(state.folders.space_folder("w2"), Some("ASOUP/THEX"));

    // A folder cannot go into itself.
    compose(&mut state);
    let from = folder_at(&state, FolderSection::Spaces, "ASOUP");
    let to = folder_at(&state, FolderSection::Spaces, "ASOUP/THEX");
    drag(&mut state, from, to);
    assert_eq!(state.folders.space_folder("w2"), Some("ASOUP/THEX"));

    compose(&mut state);
    let from = folder_at(&state, FolderSection::Spaces, "ASOUP/THEX");
    let header = state.hits.spaces_header;
    drag(&mut state, from, (header.x + 2, header.y));
    assert_eq!(state.folders.space_folder("w2"), Some("THEX"));
}

#[test]
fn move_to_folder_dialog_files_a_space_into_nested_folders() {
    let mut state = state_with(SidebarFolders::default());
    compose(&mut state);
    let at = space_at(&state, "w3");
    mouse(&mut state, MouseEventKind::Down(MouseButton::Right), at);
    choose(&mut state, ClientContextMenuAction::MoveToFolder);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::MoveToFolder { .. },
            ..
        }))
    ));
    state.handle_input_bytes(b"ASOUP / THEX");
    let saved = state.handle_input_bytes(b"\r");
    assert!(saved.actions.is_empty());
    assert!(state.overlay.is_none());
    assert_eq!(state.folders.space_folder("w3"), Some("ASOUP/THEX"));

    // The folder menu makes subfolders and deletes folders into their parent.
    compose(&mut state);
    let at = folder_at(&state, FolderSection::Spaces, "ASOUP/THEX");
    mouse(&mut state, MouseEventKind::Down(MouseButton::Right), at);
    choose(&mut state, ClientContextMenuAction::DeleteFolder);
    assert_eq!(state.folders.space_folder("w3"), Some("ASOUP"));
}

fn picker_lines(state: &mut ClientShellState) -> Vec<String> {
    let text = compose(state);
    let lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
    state
        .hits
        .folder_choice_rows
        .iter()
        .map(|(rect, _)| {
            lines[rect.y as usize]
                .chars()
                .skip(rect.x as usize)
                .take(rect.width as usize)
                .collect::<String>()
                .trim()
                .to_owned()
        })
        .collect()
}

fn open_space_picker(state: &mut ClientShellState, workspace_id: &str) {
    compose(state);
    let at = space_at(state, workspace_id);
    mouse(state, MouseEventKind::Down(MouseButton::Right), at);
    choose(state, ClientContextMenuAction::MoveToFolder);
}

#[test]
fn folder_picker_lists_existing_folders_and_moves_with_the_arrows() {
    let mut state = state_with(filed(&[("w1", "ASOUP/THEX"), ("w2", "home")]));
    open_space_picker(&mut state, "w2");
    let text = compose(&mut state);
    assert!(text.contains("move «thex-core» to folder"), "{text}");
    let lines = picker_lines(&mut state);
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert_eq!(lines[0], "⌂ top level");
    assert_eq!(lines[1], "▸ ASOUP");
    assert_eq!(lines[2], "▸ THEX");
    assert!(lines[3].starts_with("▸ home") && lines[3].ends_with("current"));

    // The current folder is highlighted first; Up reaches THEX.
    state.handle_input_bytes(b"\x1b[A");
    let saved = state.handle_input_bytes(b"\r");
    assert!(saved.actions.is_empty());
    assert!(state.overlay.is_none());
    assert_eq!(state.folders.space_folder("w2"), Some("ASOUP/THEX"));
}

#[test]
fn folder_picker_searches_and_offers_a_new_folder() {
    let mut state = state_with(filed(&[("w1", "ASOUP/THEX")]));
    open_space_picker(&mut state, "w3");
    state.handle_input_bytes(b"thex");
    assert_eq!(
        picker_lines(&mut state),
        ["▸ ASOUP/THEX", "+ new folder thex"]
    );
    // Typing picks the best match.
    state.handle_input_bytes(b"\r");
    assert_eq!(state.folders.space_folder("w3"), Some("ASOUP/THEX"));

    // Down reaches the new folder.
    open_space_picker(&mut state, "w3");
    state.handle_input_bytes(b"Later");
    state.handle_input_bytes(b"\x1b[B");
    state.handle_input_bytes(b"\r");
    assert_eq!(state.folders.space_folder("w3"), Some("Later"));
}

#[test]
fn clicking_a_picker_line_moves_at_once() {
    let mut state = state_with(filed(&[("w1", "Work")]));
    open_space_picker(&mut state, "w3");
    picker_lines(&mut state);
    let (rect, _) = state.hits.folder_choice_rows[1];
    // A click inside the picker but off its lines keeps it open.
    let popup = state.hits.folder_picker_popup;
    click(&mut state, (popup.x + 1, popup.y + 1));
    assert!(state.overlay.is_some());
    click(&mut state, (rect.x + 2, rect.y));
    assert!(state.overlay.is_none());
    assert_eq!(state.folders.space_folder("w3"), Some("Work"));
}

#[test]
fn dragging_shows_what_moves_and_where() {
    let mut folders = SidebarFolders::default();
    folders.create("Later");
    let mut state = state_with(folders);
    compose(&mut state);
    let from = agent_at(&state, "w3:p1");
    let to = folder_at(&state, FolderSection::Agents, "Later");
    mouse(&mut state, MouseEventKind::Down(MouseButton::Left), from);
    mouse(&mut state, MouseEventKind::Drag(MouseButton::Left), to);
    let text = compose(&mut state);
    assert!(text.contains("«home · battery» → ▸ Later"), "{text}");
    mouse(&mut state, MouseEventKind::Up(MouseButton::Left), to);
    let text = compose(&mut state);
    assert!(!text.contains("→ ▸ Later"), "{text}");
}

#[test]
fn filter_narrows_both_lists_while_typing_and_clears_from_the_header() {
    let mut state = state_with(filed(&[("w2", "ASOUP/THEX")]));
    compose(&mut state);
    let button = state.hits.sidebar_filter_button;
    click(&mut state, (button.x + 1, button.y));
    state.handle_input_bytes(b"thex");
    // The lists follow the words as they are typed.
    compose(&mut state);
    assert_eq!(shown_spaces(&state), ["w2"]);
    assert_eq!(shown_agents(&state), ["w2:p1"]);
    state.handle_input_bytes(b"\r");
    assert_eq!(state.sidebar_filter, "thex");
    let text = compose(&mut state);
    assert!(text.contains("⌕ thex"), "{text}");
    assert_eq!(shown_spaces(&state), ["w2"]);

    // Every word must match; states and TODO marks are words too.
    state.sidebar_filter = "review idle".into();
    compose(&mut state);
    assert_eq!(shown_agents(&state), ["w2:p1", "w1:p1"]);

    let button = state.hits.sidebar_filter_button;
    click(&mut state, (button.x + 1, button.y));
    assert!(state.sidebar_filter.is_empty());
    compose(&mut state);
    assert_eq!(shown_spaces(&state).len(), 3);
}

#[test]
fn shown_agents_move_into_a_folder_together() {
    let mut state = state_with(SidebarFolders::default());
    state.sidebar_filter = "review".into();
    compose(&mut state);
    let header = state.hits.agents_header;
    mouse(
        &mut state,
        MouseEventKind::Down(MouseButton::Right),
        (header.x + 2, header.y),
    );
    choose(&mut state, ClientContextMenuAction::MoveShownToFolder);
    state.handle_input_bytes(b"Reviews");
    state.handle_input_bytes(b"\r");
    assert_eq!(state.folders.agent_folder("w1:p1", "w1"), Some("Reviews"));
    assert_eq!(state.folders.agent_folder("w2:p1", "w2"), Some("Reviews"));
    assert_eq!(state.folders.agent_folder("w3:p1", "w3"), None);
}

#[test]
fn folders_are_kept_in_chrome_preferences() {
    let path =
        std::env::temp_dir().join(format!("herdr-sidebar-folders-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut state = state_with(filed(&[("w2", "ASOUP/THEX")]));
    state.config.preferences_path = Some(path.clone());
    compose(&mut state);
    let work = folder_at(&state, FolderSection::Spaces, "ASOUP");
    click(&mut state, work);
    let saved = crate::client::shell::preferences::load(&path).expect("saved preferences");
    assert_eq!(saved.folders.space_folder("w2"), Some("ASOUP/THEX"));
    assert!(saved.folders.collapsed_spaces.contains("ASOUP"));
    std::fs::remove_file(path).expect("remove preferences");

    // Nothing is written for a sidebar without folders.
    let empty = serde_json::to_string(
        &crate::client::shell::preferences::ClientChromePreferences::default(),
    )
    .unwrap();
    assert!(!empty.contains("folders"), "{empty}");
}

#[test]
fn spaces_seen_closing_are_forgotten_but_missing_ones_are_kept() {
    let mut folders = filed(&[("w2", "ASOUP"), ("w9", "Later")]);
    folders.file_agent("w2:p1", "w2", Some("Agents"));
    folders.file_agent("w9:p1", "w9", Some("Elsewhere"));
    let mut state = state_with(folders);
    // w9 never showed up (a restart that has not brought it back): kept.
    assert_eq!(state.folders.space_folder("w9"), Some("Later"));
    assert!(state.folders.agent_filed_apart("w9:p1"));

    // w2 closes while the client watches: forgotten with its agent.
    let mut snapshot = state.snapshot.clone().unwrap();
    snapshot
        .workspaces
        .retain(|workspace| workspace.workspace_id != "w2");
    snapshot.agents.retain(|agent| agent.workspace_id != "w2");
    state.set_snapshot(snapshot);
    assert_eq!(state.folders.space_folder("w2"), None);
    assert!(!state.folders.agent_filed_apart("w2:p1"));
    assert_eq!(state.folders.space_folder("w9"), Some("Later"));
}

#[test]
fn slash_in_navigate_mode_opens_the_filter() {
    let mut state = state_with(SidebarFolders::default());
    compose(&mut state);
    state.mode = ClientShellMode::Navigate;
    state.handle_input_bytes(b"/");
    assert_eq!(state.mode, ClientShellMode::Terminal);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::SidebarFilter,
            ..
        }))
    ));
    state.handle_input_bytes(b"home");
    compose(&mut state);
    assert_eq!(shown_spaces(&state), ["w3"]);
    // Esc keeps the filter that was there before.
    state.handle_input_bytes(b"\x1b");
    assert!(state.overlay.is_none());
    compose(&mut state);
    assert_eq!(shown_spaces(&state).len(), 3);
    assert!(state.sidebar_filter.is_empty());
}
