//! What a person does with sidebar folders: the dialogs, the context-menu
//! actions and dropping dragged spaces, agents and folders.

use super::folders::{self, FolderChoice, FolderSection, SidebarFilter};
use super::*;

pub(super) const MOVE_TO_FOLDER_TITLE: &str = "move to folder";
pub(super) const NEW_FOLDER_TITLE: &str = "new folder (a/b makes nested folders)";
pub(super) const RENAME_FOLDER_TITLE: &str = "rename or move folder (a/b nests it)";
pub(super) const FILTER_TITLE: &str = "filter spaces and agents: words, todo, blocked";

impl ClientShellState {
    /// Folders show in the one-server sidebar; the sidebar of several machines
    /// keeps its plain lists.
    pub(super) fn sidebar_has_folders(&self) -> bool {
        self.endpoints.len() <= 1 && !self.mobile_layout_active()
    }

    /// The filter words in effect: what is being typed while the filter
    /// dialog is open.
    pub(super) fn active_sidebar_filter_text(&self) -> &str {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                target: ClientRenameTarget::SidebarFilter,
                input,
                ..
            })) => input,
            _ => &self.sidebar_filter,
        }
    }

    pub(super) fn active_sidebar_filter(&self) -> SidebarFilter {
        SidebarFilter::new(self.active_sidebar_filter_text())
    }

    /// The spaces the sidebar shows, in its order, for keyboard navigation.
    pub(super) fn sidebar_space_entries(
        &self,
        snapshot: &ClientShellSnapshot,
        collapsed_groups: &HashSet<String>,
    ) -> Vec<WorkspaceEntry> {
        if !self.sidebar_has_folders() {
            return render::workspace_entries(snapshot, collapsed_groups);
        }
        folders::visible_space_entries(&folders::spaces_rows(
            snapshot,
            collapsed_groups,
            &self.folders,
            &self.active_sidebar_filter(),
        ))
    }

    /// The sidebar row of a space, folder rows counted: the scroll offset that
    /// shows it on top.
    pub(super) fn sidebar_space_row(
        &self,
        snapshot: &ClientShellSnapshot,
        workspace_id: &str,
    ) -> Option<usize> {
        let empty = HashSet::new();
        let collapsed_groups = self
            .collapsed_groups_for_endpoint(&self.active_endpoint_id)
            .unwrap_or(&empty);
        folders::spaces_rows(
            snapshot,
            collapsed_groups,
            &self.folders,
            &self.active_sidebar_filter(),
        )
        .iter()
        .position(|row| match row {
            folders::SpacesRow::Space { entry, .. } => snapshot
                .workspaces
                .get(entry.index)
                .is_some_and(|workspace| workspace.workspace_id == workspace_id),
            folders::SpacesRow::Folder(_) => false,
        })
    }

    /// The agents-list row of an agent, folder rows counted.
    pub(super) fn sidebar_agent_row(
        &self,
        snapshot: &ClientShellSnapshot,
        pane_id: &str,
    ) -> Option<usize> {
        if !self.sidebar_has_folders() || snapshot.agent_view_label.is_some() {
            return None;
        }
        folders::agent_panel_rows(
            snapshot,
            &self.folders,
            self.agents_by_priority(),
            &self.active_sidebar_filter(),
        )
            .iter()
            .position(|row| {
                matches!(row, folders::AgentListRow::Agent { pane_id: candidate, .. } if candidate == pane_id)
            })
    }

    /// Agents in folder order: what agent numbers and next/previous agent use.
    pub(super) fn sidebar_agent_order(&self, snapshot: &ClientShellSnapshot) -> Vec<String> {
        if !self.sidebar_has_folders() || snapshot.agent_view_label.is_some() {
            return super::agent_sidebar::ordered_agent_pane_ids(
                snapshot,
                self.config.agent_panel_sort,
            );
        }
        folders::agent_panel_order(snapshot, &self.folders, self.agents_by_priority())
    }

    pub(super) fn open_move_to_folder(&mut self, item: FolderItem) {
        let current = match &item {
            FolderItem::Space { workspace_id } => {
                let root = self.space_root(workspace_id);
                self.folders.space_folder(&root).map(str::to_owned)
            }
            FolderItem::Agent {
                pane_id,
                workspace_id,
            } => self
                .folders
                .agent_folder(pane_id, workspace_id)
                .map(str::to_owned),
            FolderItem::Shown(_) => None,
            FolderItem::Folder { path } => folders::folder_parent(path).map(str::to_owned),
        };
        let mut choices =
            folders::folders_in_tree_order(self.folders.all_paths(), &self.folders.order);
        if let FolderItem::Folder { path } = &item {
            choices.retain(|candidate| !folders::folder_contains(path, candidate));
        }
        let label = self.folder_item_label(&item);
        let selected = Some(match &current {
            Some(path) => FolderChoice::Folder(path.clone()),
            None => FolderChoice::TopLevel,
        });
        self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title: MOVE_TO_FOLDER_TITLE,
            input: TextEditor::new("", false),
            target: ClientRenameTarget::MoveToFolder {
                item,
                label,
                folders: choices,
                current,
                selected,
            },
        }));
    }

    /// How the folder picker and the drag label name a space, agent or the
    /// shown rows: quoted and cut to a readable length.
    fn folder_item_label(&self, item: &FolderItem) -> String {
        let snapshot = self.snapshot.as_deref();
        let text = match item {
            FolderItem::Space { workspace_id } => snapshot
                .and_then(|snapshot| {
                    snapshot
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.workspace_id == *workspace_id)
                })
                .map(|workspace| workspace.label.clone())
                .unwrap_or_else(|| workspace_id.clone()),
            FolderItem::Agent { pane_id, .. } => snapshot
                .and_then(|snapshot| agent_drag_label(snapshot, pane_id))
                .unwrap_or_else(|| pane_id.clone()),
            FolderItem::Shown(section) => {
                let count = self.shown_folder_items(*section).len();
                let noun = match (section, count) {
                    (FolderSection::Spaces, 1) => "space",
                    (FolderSection::Spaces, _) => "spaces",
                    (FolderSection::Agents, 1) => "agent",
                    (FolderSection::Agents, _) => "agents",
                };
                return format!("{count} shown {noun}");
            }
            FolderItem::Folder { path } => {
                return format!("▸ {}", cut_label(folders::folder_name(path), 36));
            }
        };
        format!("«{}»", cut_label(&text, 36))
    }

    /// Moves the folder picker's highlight by `delta` lines.
    pub(super) fn move_folder_choice(&mut self, delta: isize) -> bool {
        let Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            input,
            target:
                ClientRenameTarget::MoveToFolder {
                    folders, selected, ..
                },
            ..
        })) = self.overlay.as_mut()
        else {
            return false;
        };
        let choices = folders::folder_choices(folders, input);
        if choices.is_empty() {
            return true;
        }
        let index = folders::selected_choice_index(&choices, selected.as_ref());
        let index = index.saturating_add_signed(delta).min(choices.len() - 1);
        *selected = Some(choices[index].clone());
        true
    }

    /// Forgets the arrow pick after the typed words change, so the best match
    /// is highlighted again.
    pub(super) fn reset_folder_choice(&mut self) {
        if let Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            target: ClientRenameTarget::MoveToFolder { selected, .. },
            ..
        })) = self.overlay.as_mut()
        {
            *selected = None;
        }
    }

    /// A click on a folder picker line: moves there at once.
    pub(super) fn pick_folder_choice(&mut self, index: usize, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            input,
            target:
                ClientRenameTarget::MoveToFolder {
                    folders, selected, ..
                },
            ..
        })) = self.overlay.as_mut()
        else {
            return;
        };
        let Some(choice) = folders::folder_choices(folders, input).get(index).cloned() else {
            return;
        };
        *selected = Some(choice);
        self.save_rename_overlay(outcome);
    }

    /// The label drawn beside the pointer while a space, agent or folder is
    /// dragged: what moves and where it lands.
    pub(super) fn drag_ghost_text(&self) -> Option<String> {
        let (what, target) = match self.chrome_drag.as_ref()? {
            ClientChromeDrag::Workspace {
                source_workspace_id,
                target,
            } => {
                let what = self.folder_item_label(&FolderItem::Space {
                    workspace_id: source_workspace_id.clone(),
                });
                let target = match target {
                    None => None,
                    Some(WorkspaceDropTarget::Folder(path)) => Some(format!("▸ {path}")),
                    Some(WorkspaceDropTarget::Slot { folder, .. }) => Some(match folder {
                        Some(FolderDropTarget::Folder(path)) => format!("▸ {path}"),
                        Some(FolderDropTarget::TopLevel) => "⌂ top level".to_owned(),
                        None => "here".to_owned(),
                    }),
                };
                (what, target)
            }
            ClientChromeDrag::Agent { pane_id, target } => {
                let what = self.folder_item_label(&FolderItem::Agent {
                    pane_id: pane_id.clone(),
                    workspace_id: String::new(),
                });
                (what, target.as_ref().map(drop_target_text))
            }
            ClientChromeDrag::Folder { path, target } => (
                format!("▸ {}", cut_label(folders::folder_name(path), 24)),
                target.as_ref().map(|target| match target {
                    FolderMove::Into(FolderDropTarget::Folder(parent)) => {
                        format!("into ▸ {parent}")
                    }
                    FolderMove::Into(FolderDropTarget::TopLevel) => "⌂ top level".to_owned(),
                    FolderMove::Beside { sibling, after } => format!(
                        "{} ▸ {}",
                        if *after { "after" } else { "before" },
                        folders::folder_name(sibling)
                    ),
                }),
            ),
            _ => return None,
        };
        Some(match target {
            Some(target) => format!(" {what} → {} ", cut_label(&target, 40)),
            None => format!(" {what} ✕ "),
        })
    }

    fn open_folder_dialog(&mut self, title: &'static str, text: &str, target: ClientRenameTarget) {
        self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
            title,
            input: TextEditor::new(text, false),
            target,
        }));
    }

    pub(super) fn open_sidebar_filter(&mut self) {
        let current = self.sidebar_filter.clone();
        self.open_folder_dialog(FILTER_TITLE, &current, ClientRenameTarget::SidebarFilter);
    }

    fn space_root(&self, workspace_id: &str) -> String {
        self.snapshot
            .as_deref()
            .map(|snapshot| folders::space_block_root(snapshot, workspace_id))
            .unwrap_or_else(|| workspace_id.to_owned())
    }

    fn file_item(&mut self, item: &FolderItem, folder: Option<&str>) {
        match item {
            FolderItem::Space { workspace_id } => {
                let root = self.space_root(workspace_id);
                self.folders.file_space(&root, folder);
            }
            FolderItem::Agent {
                pane_id,
                workspace_id,
            } => self.folders.file_agent(pane_id, workspace_id, folder),
            FolderItem::Shown(section) => {
                for item in self.shown_folder_items(*section) {
                    self.file_item(&item, folder);
                }
            }
            FolderItem::Folder { path } => {
                self.folders.move_folder_into(path, folder);
            }
        }
    }

    /// The spaces or agents the sidebar filter shows now.
    fn shown_folder_items(&self, section: FolderSection) -> Vec<FolderItem> {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return Vec::new();
        };
        let filter = SidebarFilter::new(&self.sidebar_filter);
        match section {
            FolderSection::Spaces => folders::visible_space_entries(&folders::spaces_rows(
                snapshot,
                &HashSet::new(),
                &self.folders,
                &filter,
            ))
            .into_iter()
            .filter(|entry| !entry.indented)
            .filter_map(|entry| snapshot.workspaces.get(entry.index))
            .map(|workspace| FolderItem::Space {
                workspace_id: workspace.workspace_id.clone(),
            })
            .collect(),
            FolderSection::Agents => folders::agent_panel_rows(
                snapshot,
                &self.folders,
                self.agents_by_priority(),
                &filter,
            )
            .into_iter()
            .filter_map(|row| match row {
                folders::AgentListRow::Agent { pane_id, .. } => snapshot
                    .agents
                    .iter()
                    .find(|agent| agent.pane_id == pane_id)
                    .map(|agent| FolderItem::Agent {
                        pane_id,
                        workspace_id: agent.workspace_id.clone(),
                    }),
                folders::AgentListRow::Folder(_) | folders::AgentListRow::Group { .. } => None,
            })
            .collect(),
        }
    }

    /// Saves a folder dialog; false when the dialog is not about folders.
    pub(super) fn save_folder_dialog(
        &mut self,
        target: &ClientRenameTarget,
        text: &str,
        outcome: &mut ClientShellInput,
    ) -> bool {
        match target {
            ClientRenameTarget::MoveToFolder {
                item,
                folders,
                selected,
                ..
            } => {
                let choices = folders::folder_choices(folders, text);
                let Some(choice) =
                    choices.get(folders::selected_choice_index(&choices, selected.as_ref()))
                else {
                    return true;
                };
                if let FolderChoice::New(path) = choice {
                    self.folders.create(path);
                }
                self.file_item(item, choice.folder());
            }
            ClientRenameTarget::NewFolder { parent } => {
                let Some(name) = folders::normalize_folder_path(text) else {
                    return true;
                };
                let path = match parent {
                    Some(parent) => format!("{parent}/{name}"),
                    None => name,
                };
                self.folders.create(&path);
                for section in [FolderSection::Spaces, FolderSection::Agents] {
                    if let Some(parent) = parent {
                        self.expand_folder(section, parent);
                    }
                }
            }
            ClientRenameTarget::RenameFolder { path } => {
                let Some(new_path) = folders::normalize_folder_path(text) else {
                    return true;
                };
                self.folders.rename(path, &new_path);
            }
            ClientRenameTarget::SidebarFilter => {
                self.sidebar_filter = text.trim().to_owned();
                self.workspace_scroll = 0;
                self.agent_scroll = 0;
            }
            _ => return false,
        }
        self.persist_chrome_preferences(outcome);
        true
    }

    fn expand_folder(&mut self, section: FolderSection, path: &str) {
        if self.folders.collapsed(section).contains(path) {
            self.folders.toggle_collapsed(section, path);
        }
    }

    pub(super) fn toggle_folder(
        &mut self,
        section: FolderSection,
        path: &str,
        outcome: &mut ClientShellInput,
    ) {
        self.folders.toggle_collapsed(section, path);
        self.persist_chrome_preferences(outcome);
        outcome.repaint = true;
    }

    pub(super) fn activate_folder_action(
        &mut self,
        section: FolderSection,
        path: Option<String>,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        match action {
            ClientContextMenuAction::NewFolder => self.open_folder_dialog(
                NEW_FOLDER_TITLE,
                "",
                ClientRenameTarget::NewFolder { parent: path },
            ),
            ClientContextMenuAction::RenameFolder => {
                if let Some(path) = path {
                    self.open_folder_dialog(
                        RENAME_FOLDER_TITLE,
                        &path.clone(),
                        ClientRenameTarget::RenameFolder { path },
                    );
                }
            }
            ClientContextMenuAction::DeleteFolder => {
                if let Some(path) = path {
                    self.folders.delete(&path);
                    self.persist_chrome_preferences(outcome);
                }
            }
            ClientContextMenuAction::ShowOnlyFolder => {
                if let Some(path) = path {
                    self.sidebar_filter = path;
                    self.workspace_scroll = 0;
                    self.agent_scroll = 0;
                    self.persist_chrome_preferences(outcome);
                }
            }
            ClientContextMenuAction::MoveFolderUp | ClientContextMenuAction::MoveFolderDown => {
                let delta = if action == ClientContextMenuAction::MoveFolderUp {
                    -1
                } else {
                    1
                };
                if path.is_some_and(|path| self.folders.shift_folder(&path, delta)) {
                    self.persist_chrome_preferences(outcome);
                }
            }
            ClientContextMenuAction::MoveToFolder => {
                if let Some(path) = path {
                    self.open_move_to_folder(FolderItem::Folder { path });
                }
            }
            ClientContextMenuAction::ToggleAgentView => self.toggle_agent_view(outcome),
            ClientContextMenuAction::FilterSidebar => self.open_sidebar_filter(),
            ClientContextMenuAction::MoveShownToFolder => {
                self.open_move_to_folder(FolderItem::Shown(section))
            }
            ClientContextMenuAction::ClearSidebarFilter => {
                self.sidebar_filter.clear();
                self.persist_chrome_preferences(outcome);
            }
            ClientContextMenuAction::ExpandAllFolders
            | ClientContextMenuAction::CollapseAllFolders => {
                self.folders.set_all_collapsed(
                    section,
                    action == ClientContextMenuAction::CollapseAllFolders,
                );
                self.persist_chrome_preferences(outcome);
            }
            _ => {}
        }
        outcome.repaint = true;
    }

    /// The agents list sorted by priority: with folders, the view of waiting,
    /// working and other agents in place of the folder tree.
    pub(super) fn agents_by_priority(&self) -> bool {
        self.config.agent_panel_sort == crate::config::AgentPanelSortConfig::Priority
    }

    /// Switches the agents list between its two views: folders (or grouped by
    /// space without folders) and priority. Kept in the chrome preferences.
    pub(super) fn toggle_agent_view(&mut self, outcome: &mut ClientShellInput) {
        self.config.agent_panel_sort = if self.agents_by_priority() {
            crate::config::AgentPanelSortConfig::Spaces
        } else {
            crate::config::AgentPanelSortConfig::Priority
        };
        self.agent_panel_sort_manual = true;
        self.agent_scroll = 0;
        self.persist_chrome_preferences(outcome);
        outcome.repaint = true;
    }

    fn folder_hit_at(&self, point: (u16, u16)) -> Option<&FolderHit> {
        self.hits
            .folders
            .iter()
            .find(|hit| super::contains(hit.rect, point))
    }

    /// The folder a sidebar point stands for while an agent or a folder is
    /// dragged: a folder row, the folder of the row under the pointer, or the
    /// top level on a header or below the rows.
    pub(super) fn sidebar_folder_target_at(&self, point: (u16, u16)) -> Option<FolderDropTarget> {
        let folder_target = |folder: Option<&str>| match folder {
            Some(path) => FolderDropTarget::Folder(path.to_owned()),
            None => FolderDropTarget::TopLevel,
        };
        if let Some(hit) = self.folder_hit_at(point) {
            return Some(FolderDropTarget::Folder(hit.path.clone()));
        }
        if super::contains(self.hits.spaces_header, point)
            || super::contains(self.hits.agents_header, point)
        {
            return Some(FolderDropTarget::TopLevel);
        }
        let snapshot = self.snapshot.as_deref()?;
        if let Some((_, pane_id)) = self
            .hits
            .agents
            .iter()
            .find(|(rect, _)| super::contains(*rect, point))
        {
            let agent = snapshot
                .agents
                .iter()
                .find(|agent| &agent.pane_id == pane_id)?;
            return Some(folder_target(
                self.folders.agent_folder(pane_id, &agent.workspace_id),
            ));
        }
        if let Some(hit) = self
            .hits
            .workspaces
            .iter()
            .find(|hit| super::contains(hit.rect, point))
        {
            let root = folders::space_block_root(snapshot, &hit.workspace_id);
            return Some(folder_target(self.folders.space_folder(&root)));
        }
        (super::contains(self.hits.agent_body, point)
            || super::contains(self.hits.workspace_body, point))
        .then_some(FolderDropTarget::TopLevel)
    }

    /// Where a dragged folder would land: beside a sibling folder row (before
    /// it from below, after it from above), into any other folder, or into the
    /// folder of the row under the pointer. None where it cannot go (into
    /// itself) or would not move.
    pub(super) fn folder_drag_target_at(
        &self,
        path: &str,
        point: (u16, u16),
    ) -> Option<FolderMove> {
        if let Some(hit) = self.folder_hit_at(point) {
            let target = hit.path.as_str();
            if folders::folder_contains(path, target) {
                return None;
            }
            let parent = folders::folder_parent(path);
            if folders::folder_parent(target) == parent {
                let siblings = self.folders.sibling_folders(parent);
                let position = |path: &str| siblings.iter().position(|sibling| sibling == path);
                let after = position(path) < position(target);
                return Some(FolderMove::Beside {
                    sibling: target.to_owned(),
                    after,
                });
            }
        }
        self.folder_into_target_at(path, point)
            .map(FolderMove::Into)
    }

    fn folder_into_target_at(&self, path: &str, point: (u16, u16)) -> Option<FolderDropTarget> {
        match self.sidebar_folder_target_at(point)? {
            FolderDropTarget::Folder(target)
                if folders::folder_contains(path, &target)
                    || folders::folder_parent(path) == Some(target.as_str()) =>
            {
                None
            }
            FolderDropTarget::TopLevel if folders::folder_parent(path).is_none() => None,
            target => Some(target),
        }
    }

    /// Where a dragged space would land in a sidebar with folders: into the
    /// folder under the pointer, or on the line before a space (taking that
    /// space's folder) or after the last row (the top level).
    pub(super) fn folder_workspace_drop_target_at(
        &self,
        point: (u16, u16),
    ) -> Option<WorkspaceDropTarget> {
        if self.hits.workspace_body.height == 0 {
            return None;
        }
        if let Some(hit) = self
            .folder_hit_at(point)
            .filter(|hit| hit.section == FolderSection::Spaces)
        {
            return Some(WorkspaceDropTarget::Folder(hit.path.clone()));
        }
        if point.1 < self.hits.workspace_body.y.saturating_sub(1)
            || point.1 >= self.hits.new_workspace.y
        {
            return None;
        }
        let snapshot = self.snapshot.as_deref()?;
        let slot_for = |workspace_id: Option<&str>, row: u16| {
            let folder = workspace_id.and_then(|id| self.folders.space_folder(id));
            let depth = folder.map_or(0, |path| path.split('/').count() as u16);
            (
                workspace_id.map(str::to_owned),
                row,
                depth * 2,
                match folder {
                    Some(path) => FolderDropTarget::Folder(path.to_owned()),
                    None => FolderDropTarget::TopLevel,
                },
            )
        };
        let mut slots = self
            .hits
            .workspaces
            .iter()
            .filter(|hit| hit.endpoint_id == self.active_endpoint_id && !hit.indented)
            .filter(|hit| {
                hit.group_toggle.as_ref().is_none_or(|(_, key)| {
                    snapshot
                        .workspaces
                        .iter()
                        .find(|workspace| {
                            workspace.worktree.as_ref().is_some_and(|worktree| {
                                worktree.key == *key && !worktree.is_linked_worktree
                            })
                        })
                        .is_some_and(|workspace| workspace.workspace_id == hit.workspace_id)
                })
            })
            .map(|hit| slot_for(Some(&hit.workspace_id), hit.rect.y.saturating_sub(1)))
            .collect::<Vec<_>>();
        let last_row = self
            .hits
            .workspaces
            .iter()
            .map(|hit| hit.rect.bottom())
            .chain(
                self.hits
                    .folders
                    .iter()
                    .filter(|hit| hit.section == FolderSection::Spaces)
                    .map(|hit| hit.rect.bottom()),
            )
            .max();
        if let Some(row) = last_row.filter(|row| *row < self.hits.new_workspace.y) {
            slots.push(slot_for(None, row));
        }
        slots
            .into_iter()
            .enumerate()
            .min_by_key(|(index, (_, row, _, _))| (point.1.abs_diff(*row), *index))
            .map(
                |(_, (before_workspace_id, row, indent, folder))| WorkspaceDropTarget::Slot {
                    before_workspace_id,
                    row,
                    indent,
                    folder: Some(folder),
                },
            )
    }

    /// The first space filed directly in a folder, other than `source`.
    pub(super) fn first_space_in_folder(&self, path: &str, source: &str) -> Option<String> {
        let snapshot = self.snapshot.as_deref()?;
        let source_root = folders::space_block_root(snapshot, source);
        render::workspace_entries(snapshot, &HashSet::new())
            .into_iter()
            .filter(|entry| !entry.indented)
            .filter_map(|entry| snapshot.workspaces.get(entry.index))
            .map(|workspace| workspace.workspace_id.as_str())
            .find(|workspace_id| {
                *workspace_id != source_root
                    && self.folders.space_folder(workspace_id) == Some(path)
            })
            .map(str::to_owned)
    }

    pub(super) fn drop_agent(
        &mut self,
        pane_id: &str,
        target: FolderDropTarget,
        outcome: &mut ClientShellInput,
    ) {
        let Some(workspace_id) = self.snapshot.as_deref().and_then(|snapshot| {
            snapshot
                .agents
                .iter()
                .find(|agent| agent.pane_id == pane_id)
                .map(|agent| agent.workspace_id.clone())
        }) else {
            return;
        };
        let folder = match &target {
            FolderDropTarget::Folder(path) => Some(path.as_str()),
            FolderDropTarget::TopLevel => None,
        };
        self.folders.file_agent(pane_id, &workspace_id, folder);
        self.persist_chrome_preferences(outcome);
    }

    pub(super) fn drop_folder(
        &mut self,
        path: &str,
        target: FolderMove,
        outcome: &mut ClientShellInput,
    ) {
        let moved = match target {
            FolderMove::Into(FolderDropTarget::Folder(parent)) => {
                self.folders.move_folder_into(path, Some(&parent))
            }
            FolderMove::Into(FolderDropTarget::TopLevel) => {
                self.folders.move_folder_into(path, None)
            }
            FolderMove::Beside { sibling, after } => {
                self.folders.place_folder(path, &sibling, after)
            }
        };
        if moved {
            self.persist_chrome_preferences(outcome);
        }
    }

    /// Files a dropped space into the folder its target stands for; the
    /// caller then moves it on the server for a slot target.
    pub(super) fn file_dropped_space(
        &mut self,
        workspace_id: &str,
        target: &WorkspaceDropTarget,
        outcome: &mut ClientShellInput,
    ) {
        let folder = match target {
            WorkspaceDropTarget::Folder(path) => Some(path.as_str()),
            WorkspaceDropTarget::Slot {
                folder: Some(FolderDropTarget::Folder(path)),
                ..
            } => Some(path.as_str()),
            WorkspaceDropTarget::Slot {
                folder: Some(FolderDropTarget::TopLevel),
                ..
            } => None,
            WorkspaceDropTarget::Slot { folder: None, .. } => return,
        };
        let root = self.space_root(workspace_id);
        if self.folders.space_folder(&root) != folder {
            self.folders.file_space(&root, folder);
            self.persist_chrome_preferences(outcome);
        }
    }
}

fn drop_target_text(target: &FolderDropTarget) -> String {
    match target {
        FolderDropTarget::Folder(path) => format!("▸ {path}"),
        FolderDropTarget::TopLevel => "⌂ top level".to_owned(),
    }
}

/// An agent as its row reads: the space, then the agent's own title.
fn agent_drag_label(snapshot: &ClientShellSnapshot, pane_id: &str) -> Option<String> {
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.pane_id == pane_id)?;
    let space = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == agent.workspace_id)
        .map(|workspace| workspace.label.as_str());
    let title = agent
        .title
        .as_deref()
        .or(agent.terminal_title_stripped.as_deref())
        .or(agent.display_agent.as_deref())
        .or(agent.name.as_deref())
        .or(agent.agent.as_deref())
        .filter(|title| !title.trim().is_empty());
    Some(match (space, title) {
        (Some(space), Some(title)) => format!("{space} · {title}"),
        (Some(space), None) => space.to_owned(),
        (None, Some(title)) => title.to_owned(),
        (None, None) => pane_id.to_owned(),
    })
}

/// Cuts a label to `max` characters, ending it with "…".
fn cut_label(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut = text.chars().take(max.saturating_sub(1)).collect::<String>();
    cut.push('…');
    cut
}

/// Draws the drag label one row under and right of the pointer, kept inside
/// the screen; returns the cells it covers.
pub(super) fn render_drag_ghost(
    buffer: &mut ratatui::buffer::Buffer,
    text: &str,
    (column, row): (u16, u16),
    palette: &crate::app::state::Palette,
) -> Rect {
    let area = buffer.area;
    let width = super::render::display_width(text).min(area.width);
    if width == 0 || area.height == 0 {
        return Rect::default();
    }
    let x = column
        .saturating_add(2)
        .min(area.right().saturating_sub(width));
    let y = if row.saturating_add(1) < area.bottom() {
        row + 1
    } else {
        row.saturating_sub(1)
    };
    let rect = Rect::new(x, y, width, 1);
    let style = ratatui::style::Style::default()
        .fg(match palette.panel_bg {
            ratatui::style::Color::Reset => palette.surface_dim,
            color => color,
        })
        .bg(palette.accent)
        .add_modifier(ratatui::style::Modifier::BOLD);
    buffer.set_style(rect, style);
    super::render::put_text(buffer, rect.x, rect.y, rect.width, text, style);
    rect
}
