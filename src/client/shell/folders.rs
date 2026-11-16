//! Sidebar folders: a person files spaces (workspaces) and agents into nested
//! folders such as "ASOUP/THEX", drags them between folders, collapses folders
//! and filters both lists. An agent sits in its space's folder until it is filed
//! somewhere else. Folders live in this client's chrome preferences, keyed by
//! public workspace and pane ids, so the server and its frozen client-shell codec
//! know nothing about them.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use super::*;
use crate::api::schema::AgentStatus;

/// The two sidebar lists folders appear in. Each keeps its own collapsed set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum FolderSection {
    Spaces,
    Agents,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct SidebarFolders {
    /// Folders made by hand, kept while empty. Levels are joined with "/".
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(super) folders: BTreeSet<String>,
    /// Public workspace id → folder.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) spaces: BTreeMap<String, String>,
    /// Public pane id → folder of an agent filed apart from its space; "" keeps
    /// it at the top level while its space sits in a folder.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) agents: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(super) collapsed_spaces: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(super) collapsed_agents: BTreeSet<String>,
}

/// Reads a folder path as typed: levels split on "/", blanks dropped. None is
/// the top level.
pub(super) fn normalize_folder_path(input: &str) -> Option<String> {
    let levels = input
        .split('/')
        .map(str::trim)
        .filter(|level| !level.is_empty())
        .collect::<Vec<_>>();
    (!levels.is_empty()).then(|| levels.join("/"))
}

pub(super) fn folder_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub(super) fn folder_parent(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(parent, _)| parent)
}

/// True when `path` is `folder` or lies inside it.
pub(super) fn folder_contains(folder: &str, path: &str) -> bool {
    path.strip_prefix(folder)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// "A", "A/B", "A/B/C" for "A/B/C".
fn folder_and_ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/')
        .map(move |(index, _)| &path[..index])
        .chain(std::iter::once(path))
}

/// Moves `path` from inside `from` to the same place inside `to` (None: the top
/// level).
fn rebase_path(path: &str, from: &str, to: Option<&str>) -> Option<String> {
    let rest = path.strip_prefix(from)?.trim_start_matches('/');
    match (to, rest.is_empty()) {
        (Some(to), true) => Some(to.to_owned()),
        (Some(to), false) => Some(format!("{to}/{rest}")),
        (None, false) => Some(rest.to_owned()),
        (None, true) => None,
    }
}

fn workspace_of_pane(pane_id: &str) -> &str {
    pane_id
        .split_once(':')
        .map_or(pane_id, |(workspace, _)| workspace)
}

impl SidebarFolders {
    pub(super) fn is_empty(&self) -> bool {
        self.folders.is_empty()
            && self.spaces.is_empty()
            && self.agents.is_empty()
            && self.collapsed_spaces.is_empty()
            && self.collapsed_agents.is_empty()
    }

    pub(super) fn space_folder(&self, workspace_id: &str) -> Option<&str> {
        self.spaces.get(workspace_id).map(String::as_str)
    }

    pub(super) fn agent_folder(&self, pane_id: &str, workspace_id: &str) -> Option<&str> {
        match self.agents.get(pane_id) {
            Some(path) => (!path.is_empty()).then_some(path.as_str()),
            None => self.space_folder(workspace_id),
        }
    }

    /// The agent was filed apart from its space.
    pub(super) fn agent_filed_apart(&self, pane_id: &str) -> bool {
        self.agents.contains_key(pane_id)
    }

    pub(super) fn file_space(&mut self, workspace_id: &str, folder: Option<&str>) {
        match folder {
            Some(folder) => {
                self.spaces
                    .insert(workspace_id.to_owned(), folder.to_owned());
            }
            None => {
                self.spaces.remove(workspace_id);
            }
        }
    }

    /// Files an agent into a folder. Filing it where its space is makes it
    /// follow the space again.
    pub(super) fn file_agent(&mut self, pane_id: &str, workspace_id: &str, folder: Option<&str>) {
        if self.space_folder(workspace_id) == folder {
            self.agents.remove(pane_id);
        } else {
            self.agents
                .insert(pane_id.to_owned(), folder.unwrap_or_default().to_owned());
        }
    }

    pub(super) fn follow_space(&mut self, pane_id: &str) {
        self.agents.remove(pane_id);
    }

    pub(super) fn create(&mut self, path: &str) {
        self.folders.insert(path.to_owned());
    }

    /// Every folder: made by hand or holding something, with all their parents.
    pub(super) fn all_paths(&self) -> BTreeSet<String> {
        self.folders
            .iter()
            .chain(self.spaces.values())
            .chain(self.agents.values())
            .filter(|path| !path.is_empty())
            .flat_map(|path| folder_and_ancestors(path))
            .map(str::to_owned)
            .collect()
    }

    /// Renames or moves a folder with everything inside it; an existing folder
    /// of the new name takes its contents. Refuses to move a folder into itself.
    pub(super) fn rename(&mut self, from: &str, to: &str) -> bool {
        if from == to {
            return false;
        }
        if folder_contains(from, to) {
            return false;
        }
        self.rebase(from, Some(to));
        self.folders.insert(to.to_owned());
        true
    }

    /// Deletes a folder: what it held moves up one level.
    pub(super) fn delete(&mut self, path: &str) {
        let parent = folder_parent(path).map(str::to_owned);
        self.folders.remove(path);
        self.collapsed_spaces.remove(path);
        self.collapsed_agents.remove(path);
        self.rebase(path, parent.as_deref());
    }

    fn rebase(&mut self, from: &str, to: Option<&str>) {
        let moved =
            |path: &String| folder_contains(from, path).then(|| rebase_path(path, from, to));
        let rebase_set = |set: &BTreeSet<String>| {
            set.iter()
                .filter_map(|path| match moved(path) {
                    Some(new) => new,
                    None => Some(path.clone()),
                })
                .collect::<BTreeSet<_>>()
        };
        self.folders = rebase_set(&self.folders);
        self.collapsed_spaces = rebase_set(&self.collapsed_spaces);
        self.collapsed_agents = rebase_set(&self.collapsed_agents);
        self.spaces = std::mem::take(&mut self.spaces)
            .into_iter()
            .filter_map(|(workspace_id, path)| match moved(&path) {
                Some(Some(new)) => Some((workspace_id, new)),
                Some(None) => None,
                None => Some((workspace_id, path)),
            })
            .collect();
        let agents = std::mem::take(&mut self.agents);
        for (pane_id, path) in agents {
            let path = match moved(&path) {
                Some(new) => new,
                None if path.is_empty() => None,
                None => Some(path),
            };
            self.file_agent(&pane_id, workspace_of_pane(&pane_id), path.as_deref());
        }
    }

    pub(super) fn collapsed(&self, section: FolderSection) -> &BTreeSet<String> {
        match section {
            FolderSection::Spaces => &self.collapsed_spaces,
            FolderSection::Agents => &self.collapsed_agents,
        }
    }

    fn collapsed_mut(&mut self, section: FolderSection) -> &mut BTreeSet<String> {
        match section {
            FolderSection::Spaces => &mut self.collapsed_spaces,
            FolderSection::Agents => &mut self.collapsed_agents,
        }
    }

    pub(super) fn toggle_collapsed(&mut self, section: FolderSection, path: &str) {
        let collapsed = self.collapsed_mut(section);
        if !collapsed.remove(path) {
            collapsed.insert(path.to_owned());
        }
    }

    pub(super) fn set_all_collapsed(&mut self, section: FolderSection, collapse: bool) {
        let paths = self.all_paths();
        let collapsed = self.collapsed_mut(section);
        if collapse {
            collapsed.extend(paths);
        } else {
            collapsed.clear();
        }
    }

    /// Forgets spaces and agents of workspaces the server no longer has, so a
    /// workspace id the server hands out again starts unfiled.
    pub(super) fn prune(&mut self, snapshot: &ClientShellSnapshot) -> bool {
        if snapshot.workspaces.is_empty() {
            return false;
        }
        let live = snapshot
            .workspaces
            .iter()
            .map(|workspace| workspace.workspace_id.as_str())
            .collect::<HashSet<_>>();
        let before = (self.spaces.len(), self.agents.len());
        self.spaces
            .retain(|workspace_id, _| live.contains(workspace_id.as_str()));
        self.agents
            .retain(|pane_id, _| live.contains(workspace_of_pane(pane_id)));
        before != (self.spaces.len(), self.agents.len())
    }

    /// Folders shown in a section without a filter: those holding something
    /// there, and empty folders, which show in both so things can be dragged in.
    fn shown_paths(
        &self,
        snapshot: &ClientShellSnapshot,
        section: FolderSection,
    ) -> BTreeSet<String> {
        let ancestors_of = |paths: Vec<&str>| {
            paths
                .into_iter()
                .flat_map(folder_and_ancestors)
                .map(str::to_owned)
                .collect::<BTreeSet<_>>()
        };
        let spaces = ancestors_of(
            snapshot
                .workspaces
                .iter()
                .filter_map(|workspace| self.space_folder(&workspace.workspace_id))
                .collect(),
        );
        let agents = ancestors_of(
            snapshot
                .agents
                .iter()
                .filter_map(|agent| self.agent_folder(&agent.pane_id, &agent.workspace_id))
                .collect(),
        );
        let (own, other) = match section {
            FolderSection::Spaces => (&spaces, &agents),
            FolderSection::Agents => (&agents, &spaces),
        };
        self.all_paths()
            .into_iter()
            .filter(|path| own.contains(path) || !other.contains(path))
            .collect()
    }
}

/// Words typed into the sidebar filter; a row shows when its text holds every
/// word, case-insensitively.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct SidebarFilter {
    terms: Vec<String>,
}

impl SidebarFilter {
    pub(super) fn new(text: &str) -> Self {
        Self {
            terms: text.split_whitespace().map(str::to_lowercase).collect(),
        }
    }

    pub(super) fn is_active(&self) -> bool {
        !self.terms.is_empty()
    }

    pub(super) fn matches(&self, text: &str) -> bool {
        let text = text.to_lowercase();
        self.terms.iter().all(|term| text.contains(term.as_str()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FolderRow {
    pub(super) path: String,
    pub(super) depth: u16,
    pub(super) collapsed: bool,
    /// Spaces or agents anywhere inside.
    pub(super) count: usize,
    /// The most urgent agent state inside.
    pub(super) status: AgentStatus,
    /// An agent inside is marked TODO.
    pub(super) todo: bool,
}

#[derive(Clone, Debug)]
pub(super) enum SpacesRow {
    Folder(FolderRow),
    Space { entry: WorkspaceEntry, depth: u16 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AgentListRow {
    Folder(FolderRow),
    Agent { pane_id: String, depth: u16 },
}

enum TreeRow<T> {
    Folder(FolderRow),
    Item { value: T, depth: u16 },
}

struct TreeItem<T> {
    folder: Option<String>,
    value: T,
    focused: bool,
    status: AgentStatus,
    todo: bool,
}

#[derive(Default)]
struct FolderStats {
    count: usize,
    status: Option<AgentStatus>,
    todo: bool,
    focused_item: Option<usize>,
}

struct Tree<'a, T> {
    items: &'a [TreeItem<T>],
    children: HashMap<Option<String>, Vec<String>>,
    by_folder: HashMap<Option<String>, Vec<usize>>,
    stats: HashMap<String, FolderStats>,
    collapsed: &'a BTreeSet<String>,
    expand_all: bool,
}

/// Lays out folders and items depth first: at each level the subfolders by
/// name, then the items in their given order. A collapsed folder still shows
/// the focused item inside it.
fn build_tree<T: Clone>(
    items: &[TreeItem<T>],
    extra_folders: BTreeSet<String>,
    collapsed: &BTreeSet<String>,
    expand_all: bool,
) -> Vec<TreeRow<T>> {
    let mut shown = BTreeSet::new();
    for path in extra_folders
        .iter()
        .map(String::as_str)
        .chain(items.iter().filter_map(|item| item.folder.as_deref()))
    {
        shown.extend(folder_and_ancestors(path).map(str::to_owned));
    }
    let mut children = HashMap::<Option<String>, Vec<String>>::new();
    for path in &shown {
        children
            .entry(folder_parent(path).map(str::to_owned))
            .or_default()
            .push(path.clone());
    }
    for siblings in children.values_mut() {
        siblings.sort_by(|left, right| {
            let (left, right) = (folder_name(left), folder_name(right));
            left.to_lowercase()
                .cmp(&right.to_lowercase())
                .then_with(|| left.cmp(right))
        });
    }
    let mut by_folder = HashMap::<Option<String>, Vec<usize>>::new();
    let mut stats = HashMap::<String, FolderStats>::new();
    for (index, item) in items.iter().enumerate() {
        by_folder
            .entry(item.folder.clone())
            .or_default()
            .push(index);
        for path in item
            .folder
            .iter()
            .flat_map(|path| folder_and_ancestors(path))
        {
            let stat = stats.entry(path.to_owned()).or_default();
            stat.count += 1;
            stat.todo |= item.todo;
            if stat
                .status
                .is_none_or(|status| status_priority(item.status) > status_priority(status))
            {
                stat.status = Some(item.status);
            }
            if item.focused {
                stat.focused_item.get_or_insert(index);
            }
        }
    }
    let tree = Tree {
        items,
        children,
        by_folder,
        stats,
        collapsed,
        expand_all,
    };
    let mut rows = Vec::new();
    walk_tree(&tree, None, 0, &mut rows);
    rows
}

fn walk_tree<T: Clone>(
    tree: &Tree<'_, T>,
    parent: Option<String>,
    depth: u16,
    rows: &mut Vec<TreeRow<T>>,
) {
    for child in tree.children.get(&parent).into_iter().flatten() {
        let stats = tree.stats.get(child);
        let collapsed = !tree.expand_all && tree.collapsed.contains(child);
        rows.push(TreeRow::Folder(FolderRow {
            path: child.clone(),
            depth,
            collapsed,
            count: stats.map_or(0, |stats| stats.count),
            status: stats
                .and_then(|stats| stats.status)
                .unwrap_or(AgentStatus::Unknown),
            todo: stats.is_some_and(|stats| stats.todo),
        }));
        if !collapsed {
            walk_tree(tree, Some(child.clone()), depth.saturating_add(1), rows);
        } else if let Some(index) = stats.and_then(|stats| stats.focused_item) {
            rows.push(TreeRow::Item {
                value: tree.items[index].value.clone(),
                depth: depth.saturating_add(1),
            });
        }
    }
    for index in tree.by_folder.get(&parent).into_iter().flatten() {
        rows.push(TreeRow::Item {
            value: tree.items[*index].value.clone(),
            depth,
        });
    }
}

fn space_text(workspace: &ClientShellWorkspace, folder: Option<&str>) -> String {
    [
        workspace.label.as_str(),
        workspace.branch.as_deref().unwrap_or_default(),
        folder.unwrap_or_default(),
    ]
    .join(" ")
}

/// The spaces list: worktree groups stay together, each filed by its first row.
pub(super) fn spaces_rows(
    snapshot: &ClientShellSnapshot,
    collapsed_groups: &HashSet<String>,
    folders: &SidebarFolders,
    filter: &SidebarFilter,
) -> Vec<SpacesRow> {
    let mut blocks = Vec::<Vec<WorkspaceEntry>>::new();
    for entry in super::sidebar::workspace_entries(snapshot, collapsed_groups) {
        match blocks.last_mut() {
            Some(block) if entry.indented => block.push(entry),
            _ => blocks.push(vec![entry]),
        }
    }
    let items = blocks
        .into_iter()
        .filter_map(|block| {
            let root = snapshot.workspaces.get(block.first()?.index)?;
            let folder = folders.space_folder(&root.workspace_id).map(str::to_owned);
            let members = block
                .iter()
                .filter_map(|entry| snapshot.workspaces.get(entry.index))
                .collect::<Vec<_>>();
            if filter.is_active()
                && !members
                    .iter()
                    .any(|workspace| filter.matches(&space_text(workspace, folder.as_deref())))
            {
                return None;
            }
            Some(TreeItem {
                folder,
                focused: members.iter().any(|workspace| workspace.focused),
                status: members
                    .iter()
                    .map(|workspace| workspace.agent_status)
                    .max_by_key(|status| status_priority(*status))
                    .unwrap_or(AgentStatus::Unknown),
                todo: false,
                value: block,
            })
        })
        .collect::<Vec<_>>();
    let extra = if filter.is_active() {
        BTreeSet::new()
    } else {
        folders.shown_paths(snapshot, FolderSection::Spaces)
    };
    build_tree(
        &items,
        extra,
        folders.collapsed(FolderSection::Spaces),
        filter.is_active(),
    )
    .into_iter()
    .flat_map(|row| match row {
        TreeRow::Folder(folder) => vec![SpacesRow::Folder(folder)],
        TreeRow::Item { value, depth } => value
            .into_iter()
            .map(|entry| SpacesRow::Space { entry, depth })
            .collect(),
    })
    .collect()
}

/// The spaces in the order the sidebar shows them, for keyboard navigation.
pub(super) fn visible_space_entries(rows: &[SpacesRow]) -> Vec<WorkspaceEntry> {
    rows.iter()
        .filter_map(|row| match row {
            SpacesRow::Space { entry, .. } => Some(*entry),
            SpacesRow::Folder(_) => None,
        })
        .collect()
}

pub(super) fn agent_is_todo(agent: &crate::protocol::ClientShellAgent) -> bool {
    agent
        .tokens
        .iter()
        .any(|(key, _)| key == crate::pane_todo::TODO_TOKEN)
}

fn agent_text(
    snapshot: &ClientShellSnapshot,
    agent: &crate::protocol::ClientShellAgent,
    folder: Option<&str>,
) -> String {
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == agent.workspace_id);
    let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == agent.tab_id);
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.pane_id == agent.pane_id);
    [
        agent.display_agent.as_deref(),
        agent.name.as_deref(),
        agent.agent.as_deref(),
        agent.title.as_deref(),
        agent.terminal_title_stripped.as_deref(),
        pane.and_then(|pane| pane.label.as_deref()),
        tab.map(|tab| tab.label.as_str()),
        workspace.map(|workspace| workspace.label.as_str()),
        Some(status_text(agent.agent_status)),
        agent_is_todo(agent).then_some("todo"),
        folder,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
}

/// The agents list in the given order, filed into folders.
pub(super) fn agent_list_rows(
    snapshot: &ClientShellSnapshot,
    order: &[String],
    folders: &SidebarFolders,
    filter: &SidebarFilter,
) -> Vec<AgentListRow> {
    let items = order
        .iter()
        .filter_map(|pane_id| {
            let agent = snapshot
                .agents
                .iter()
                .find(|agent| &agent.pane_id == pane_id)?;
            let folder = folders
                .agent_folder(&agent.pane_id, &agent.workspace_id)
                .map(str::to_owned);
            if filter.is_active()
                && !filter.matches(&agent_text(snapshot, agent, folder.as_deref()))
            {
                return None;
            }
            Some(TreeItem {
                folder,
                value: pane_id.clone(),
                focused: agent.focused,
                status: agent.agent_status,
                todo: agent_is_todo(agent),
            })
        })
        .collect::<Vec<_>>();
    let extra = if filter.is_active() {
        BTreeSet::new()
    } else {
        folders.shown_paths(snapshot, FolderSection::Agents)
    };
    build_tree(
        &items,
        extra,
        folders.collapsed(FolderSection::Agents),
        filter.is_active(),
    )
    .into_iter()
    .map(|row| match row {
        TreeRow::Folder(folder) => AgentListRow::Folder(folder),
        TreeRow::Item { value, depth } => AgentListRow::Agent {
            pane_id: value,
            depth,
        },
    })
    .collect()
}

/// Every agent in folder order, collapsed folders included: the order agent
/// numbers and next/previous agent follow.
pub(super) fn agent_order_in_folders(
    snapshot: &ClientShellSnapshot,
    order: Vec<String>,
    folders: &SidebarFolders,
) -> Vec<String> {
    if folders.is_empty() {
        return order;
    }
    let mut open = folders.clone();
    open.collapsed_agents.clear();
    agent_list_rows(snapshot, &order, &open, &SidebarFilter::default())
        .into_iter()
        .filter_map(|row| match row {
            AgentListRow::Agent { pane_id, .. } => Some(pane_id),
            AgentListRow::Folder(_) => None,
        })
        .collect()
}

/// The first row of the worktree group a workspace belongs to: the row a
/// group is filed and dragged by.
pub(super) fn space_block_root(snapshot: &ClientShellSnapshot, workspace_id: &str) -> String {
    let rows = super::sidebar::workspace_entries(snapshot, &HashSet::new());
    let mut root = None;
    for entry in rows {
        let Some(workspace) = snapshot.workspaces.get(entry.index) else {
            continue;
        };
        if !entry.indented {
            root = Some(workspace.workspace_id.as_str());
        }
        if workspace.workspace_id == workspace_id {
            return root.unwrap_or(workspace_id).to_owned();
        }
    }
    workspace_id.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_paths_are_read_as_typed() {
        assert_eq!(
            normalize_folder_path(" ASOUP / THEX/ "),
            Some("ASOUP/THEX".into())
        );
        assert_eq!(normalize_folder_path(" / "), None);
        assert_eq!(folder_name("ASOUP/THEX"), "THEX");
        assert_eq!(folder_parent("ASOUP/THEX"), Some("ASOUP"));
        assert_eq!(folder_parent("ASOUP"), None);
        assert!(folder_contains("ASOUP", "ASOUP/THEX"));
        assert!(folder_contains("ASOUP", "ASOUP"));
        assert!(!folder_contains("ASOUP", "ASOUPX"));
    }

    #[test]
    fn agents_follow_their_space_until_filed_apart() {
        let mut folders = SidebarFolders::default();
        folders.file_space("w1", Some("ASOUP/THEX"));
        assert_eq!(folders.agent_folder("w1:p1", "w1"), Some("ASOUP/THEX"));

        folders.file_agent("w1:p1", "w1", None);
        assert_eq!(folders.agent_folder("w1:p1", "w1"), None);
        assert!(folders.agent_filed_apart("w1:p1"));

        // Filing it back where its space is drops the override.
        folders.file_agent("w1:p1", "w1", Some("ASOUP/THEX"));
        assert!(!folders.agent_filed_apart("w1:p1"));
    }

    #[test]
    fn renaming_and_deleting_carry_the_contents() {
        let mut folders = SidebarFolders::default();
        folders.file_space("w1", Some("ASOUP/THEX"));
        folders.file_agent("w2:p1", "w2", Some("ASOUP/THEX/agents"));
        folders.create("ASOUP/empty");
        folders.toggle_collapsed(FolderSection::Spaces, "ASOUP/THEX");

        assert!(!folders.rename("ASOUP", "ASOUP/inner"));
        assert!(folders.rename("ASOUP", "Work"));
        assert_eq!(folders.space_folder("w1"), Some("Work/THEX"));
        assert_eq!(
            folders.agent_folder("w2:p1", "w2"),
            Some("Work/THEX/agents")
        );
        assert!(folders.folders.contains("Work/empty"));
        assert!(folders.collapsed_spaces.contains("Work/THEX"));

        folders.delete("Work/THEX");
        assert_eq!(folders.space_folder("w1"), Some("Work"));
        assert_eq!(folders.agent_folder("w2:p1", "w2"), Some("Work/agents"));
        assert!(folders.collapsed_spaces.is_empty());

        folders.delete("Work");
        assert_eq!(folders.space_folder("w1"), None);
        assert_eq!(folders.agent_folder("w2:p1", "w2"), Some("agents"));
        assert_eq!(
            folders.all_paths().into_iter().collect::<Vec<_>>(),
            vec!["agents".to_owned(), "empty".to_owned()]
        );
    }

    #[test]
    fn filter_needs_every_word() {
        let filter = SidebarFilter::new("Thex  review");
        assert!(filter.is_active());
        assert!(filter.matches("THEX code review"));
        assert!(!filter.matches("THEX code"));
        assert!(!SidebarFilter::new("  ").is_active());
    }

    fn item(folder: Option<&str>, value: &str, focused: bool) -> TreeItem<String> {
        TreeItem {
            folder: folder.map(str::to_owned),
            value: value.to_owned(),
            focused,
            status: AgentStatus::Idle,
            todo: false,
        }
    }

    fn outline(rows: &[TreeRow<String>]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                TreeRow::Folder(folder) => format!(
                    "{}{}{} ({})",
                    "  ".repeat(folder.depth as usize),
                    if folder.collapsed { "+" } else { "-" },
                    folder_name(&folder.path),
                    folder.count
                ),
                TreeRow::Item { value, depth } => {
                    format!("{}{value}", "  ".repeat(*depth as usize))
                }
            })
            .collect()
    }

    #[test]
    fn tree_puts_folders_first_and_keeps_item_order() {
        let items = [
            item(None, "loose", false),
            item(Some("ASOUP/THEX"), "thex-a", false),
            item(Some("ASOUP"), "core", false),
            item(Some("ASOUP/THEX"), "thex-b", false),
            item(Some("home"), "battery", false),
        ];
        let rows = build_tree(&items, ["Empty".to_owned()].into(), &BTreeSet::new(), false);
        assert_eq!(
            outline(&rows),
            [
                "-ASOUP (3)",
                "  -THEX (2)",
                "    thex-a",
                "    thex-b",
                "  core",
                "-Empty (0)",
                "-home (1)",
                "  battery",
                "loose",
            ]
        );
    }

    #[test]
    fn collapsed_folder_still_shows_its_focused_item() {
        let items = [
            item(Some("ASOUP/THEX"), "thex-a", false),
            item(Some("ASOUP/THEX"), "thex-b", true),
            item(Some("ASOUP"), "core", false),
        ];
        let collapsed = ["ASOUP".to_owned()].into();
        let rows = build_tree(&items, BTreeSet::new(), &collapsed, false);
        assert_eq!(outline(&rows), ["+ASOUP (3)", "  thex-b"]);

        let rows = build_tree(&items, BTreeSet::new(), &collapsed, true);
        assert_eq!(outline(&rows).len(), 5);
    }
}
