//! Sidebar folders: a person files spaces (workspaces) and agents into nested
//! folders such as "ASOUP/THEX", drags them between folders, collapses folders
//! and filters both lists. An agent sits in its space's folder until it is filed
//! somewhere else. Folders live in this client's chrome preferences, keyed by
//! public workspace and pane ids, so the server and its frozen client-shell codec
//! know nothing about them.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

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
    /// Folders in the order they were arranged by hand. Siblings missing here
    /// follow the arranged ones, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) order: Vec<String>,
    /// The sidebar filter, kept across restarts like the rest.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(super) filter: String,
    /// Folders whose agents raise no toasts, sounds or system notifications.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(super) muted: BTreeSet<String>,
    /// Groups of the priority view folded to their head ("todo", "other").
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(super) collapsed_groups: BTreeSet<String>,
    /// Public pane ids in the order agents were arranged by hand inside their
    /// folders; agents missing here follow in their spaces' order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) agent_order: Vec<String>,
    /// Keys a newer build wrote, kept as they are when this build saves.
    #[serde(flatten)]
    pub(super) unknown: serde_json::Map<String, serde_json::Value>,
}

impl SidebarFolders {
    /// `ours`'s changes since `base`, laid over `theirs` (see
    /// `preferences::merge`): one window's folders don't undo another's.
    pub(super) fn merged(base: &Self, ours: Self, theirs: Self) -> Self {
        Self {
            folders: merge_set(&base.folders, ours.folders, theirs.folders),
            spaces: merge_map(&base.spaces, ours.spaces, theirs.spaces),
            agents: merge_map(&base.agents, ours.agents, theirs.agents),
            collapsed_spaces: merge_set(
                &base.collapsed_spaces,
                ours.collapsed_spaces,
                theirs.collapsed_spaces,
            ),
            collapsed_agents: merge_set(
                &base.collapsed_agents,
                ours.collapsed_agents,
                theirs.collapsed_agents,
            ),
            order: merge_order(&base.order, ours.order, theirs.order),
            filter: super::preferences::pick(&base.filter, ours.filter, theirs.filter),
            muted: merge_set(&base.muted, ours.muted, theirs.muted),
            collapsed_groups: merge_set(
                &base.collapsed_groups,
                ours.collapsed_groups,
                theirs.collapsed_groups,
            ),
            agent_order: merge_order(&base.agent_order, ours.agent_order, theirs.agent_order),
            unknown: theirs.unknown,
        }
    }
}

/// `theirs` with what `ours` added since `base` and without what it removed.
pub(super) fn merge_set(
    base: &BTreeSet<String>,
    ours: BTreeSet<String>,
    mut theirs: BTreeSet<String>,
) -> BTreeSet<String> {
    for removed in base.difference(&ours) {
        theirs.remove(removed);
    }
    theirs.extend(ours.difference(base).cloned());
    theirs
}

fn merge_map(
    base: &BTreeMap<String, String>,
    ours: BTreeMap<String, String>,
    mut theirs: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    for key in base.keys().filter(|key| !ours.contains_key(*key)) {
        theirs.remove(key);
    }
    for (key, value) in ours {
        if base.get(&key) != Some(&value) {
            theirs.insert(key, value);
        }
    }
    theirs
}

/// A hand order this window rearranged wins; entries only the other window
/// added follow it.
fn merge_order(base: &[String], ours: Vec<String>, theirs: Vec<String>) -> Vec<String> {
    if ours == base {
        return theirs;
    }
    let mut merged = ours;
    for entry in theirs {
        if !base.contains(&entry) && !merged.contains(&entry) {
            merged.push(entry);
        }
    }
    merged
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
            && self.order.is_empty()
            && self.filter.is_empty()
            && self.muted.is_empty()
            && self.collapsed_groups.is_empty()
            && self.agent_order.is_empty()
            && self.unknown.is_empty()
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

    /// The folders directly inside `parent` (None: the top level), in the
    /// order the sidebar shows them.
    pub(super) fn sibling_folders(&self, parent: Option<&str>) -> Vec<String> {
        let mut siblings = self
            .all_paths()
            .into_iter()
            .filter(|path| folder_parent(path) == parent)
            .collect::<Vec<_>>();
        sort_siblings(&mut siblings, &self.order);
        siblings
    }

    /// Puts a folder just before or after a sibling, arranging their level by
    /// hand from then on.
    pub(super) fn place_folder(&mut self, path: &str, sibling: &str, after: bool) -> bool {
        let parent = folder_parent(path);
        if path == sibling || parent != folder_parent(sibling) {
            return false;
        }
        let mut siblings = self.sibling_folders(parent);
        let before = siblings.clone();
        siblings.retain(|candidate| candidate != path);
        let Some(index) = siblings.iter().position(|candidate| candidate == sibling) else {
            return false;
        };
        siblings.insert(index + usize::from(after), path.to_owned());
        if siblings == before && siblings.iter().all(|path| self.order.contains(path)) {
            return false;
        }
        self.order
            .retain(|ordered| folder_parent(ordered) != parent);
        self.order.extend(siblings);
        true
    }

    /// Moves a folder one place up (-1) or down (1) among its siblings.
    pub(super) fn shift_folder(&mut self, path: &str, delta: isize) -> bool {
        let siblings = self.sibling_folders(folder_parent(path));
        let Some(index) = siblings.iter().position(|candidate| candidate == path) else {
            return false;
        };
        let Some(target) = index
            .checked_add_signed(delta)
            .and_then(|target| siblings.get(target))
        else {
            return false;
        };
        self.place_folder(path, &target.clone(), delta > 0)
    }

    /// Moves a folder with its contents into `parent` (None: the top level),
    /// where it goes after the folders arranged by hand.
    pub(super) fn move_folder_into(&mut self, path: &str, parent: Option<&str>) -> bool {
        let name = folder_name(path);
        let new_path = match parent {
            Some(parent) => format!("{parent}/{name}"),
            None => name.to_owned(),
        };
        if folder_parent(path) == parent || !self.rename(path, &new_path) {
            return false;
        }
        self.order.retain(|ordered| ordered != &new_path);
        true
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
        self.order.retain(|ordered| ordered != path);
        self.muted.remove(path);
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
        let mut order = Vec::<String>::new();
        for path in std::mem::take(&mut self.order) {
            let path = match moved(&path) {
                Some(Some(new)) => new,
                Some(None) => continue,
                None => path,
            };
            if !order.contains(&path) {
                order.push(path);
            }
        }
        self.order = order;
        self.collapsed_spaces = rebase_set(&self.collapsed_spaces);
        self.collapsed_agents = rebase_set(&self.collapsed_agents);
        self.muted = rebase_set(&self.muted);
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

    /// Forgets spaces and agents of workspaces seen in an earlier snapshot and
    /// gone from this one, so a workspace id the server hands out again starts
    /// unfiled. Ids never seen are kept: after a server restart, or a restore
    /// that brings back fewer spaces, the layout must survive. Returns the
    /// workspace ids of this snapshot, the next call's `seen`.
    pub(super) fn prune(
        &mut self,
        snapshot: &ClientShellSnapshot,
        seen: &HashSet<String>,
    ) -> HashSet<String> {
        let live = snapshot
            .workspaces
            .iter()
            .map(|workspace| workspace.workspace_id.clone())
            .collect::<HashSet<_>>();
        let closed =
            |workspace_id: &str| seen.contains(workspace_id) && !live.contains(workspace_id);
        self.spaces.retain(|workspace_id, _| !closed(workspace_id));
        self.agents
            .retain(|pane_id, _| !closed(workspace_of_pane(pane_id)));
        self.agent_order
            .retain(|pane_id| !closed(workspace_of_pane(pane_id)));
        live
    }

    /// An agent's notifications are muted when its folder or a folder above
    /// it is.
    pub(super) fn agent_muted(&self, pane_id: &str, workspace_id: &str) -> bool {
        !self.muted.is_empty()
            && self
                .agent_folder(pane_id, workspace_id)
                .is_some_and(|folder| {
                    folder_and_ancestors(folder).any(|path| self.muted.contains(path))
                })
    }

    fn mark_muted(&self, mut folder: FolderRow) -> FolderRow {
        folder.muted = folder_and_ancestors(&folder.path).any(|path| self.muted.contains(path));
        folder
    }

    pub(super) fn toggle_muted(&mut self, path: &str) {
        if !self.muted.remove(path) {
            self.muted.insert(path.to_owned());
        }
    }

    pub(super) fn toggle_group(&mut self, group: AgentGroup) {
        if !self.collapsed_groups.remove(group.label()) {
            self.collapsed_groups.insert(group.label().to_owned());
        }
    }

    /// Puts an agent just before or after another in the hand-made order of
    /// `siblings`: the agents of their folder as the list shows them.
    pub(super) fn place_agent(
        &mut self,
        pane_id: &str,
        sibling: &str,
        after: bool,
        mut siblings: Vec<String>,
    ) -> bool {
        if pane_id == sibling {
            return false;
        }
        siblings.retain(|candidate| candidate != pane_id);
        let Some(index) = siblings.iter().position(|candidate| candidate == sibling) else {
            return false;
        };
        siblings.insert(index + usize::from(after), pane_id.to_owned());
        self.agent_order
            .retain(|ordered| !siblings.contains(ordered));
        self.agent_order.extend(siblings);
        true
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
    /// How many of them wait for a person (blocked, done or idle) and how many
    /// are working.
    pub(super) waiting: usize,
    pub(super) working: usize,
    /// The most urgent state among the waiting ones: what colours their count.
    pub(super) waiting_status: AgentStatus,
    /// Its notifications are muted, by itself or a folder above it.
    pub(super) muted: bool,
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
    Agent {
        pane_id: String,
        depth: u16,
    },
    /// The head of a group in the priority view.
    Group {
        group: AgentGroup,
        count: usize,
        /// The most urgent state in the group: what colours its head.
        status: AgentStatus,
        collapsed: bool,
    },
}

/// The groups of the priority view, in the order it shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum AgentGroup {
    /// Blocked, done or idle: the agent waits for a person.
    Waiting,
    Working,
    /// Marked TODO: put aside to come back to, whatever its state.
    Todo,
    /// No state known: shells and agents not yet recognised.
    Other,
}

impl AgentGroup {
    pub(super) fn of(status: AgentStatus, todo: bool) -> Self {
        match status {
            _ if todo => Self::Todo,
            AgentStatus::Blocked | AgentStatus::Done | AgentStatus::Idle => Self::Waiting,
            AgentStatus::Working => Self::Working,
            AgentStatus::Unknown => Self::Other,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Working => "working",
            Self::Todo => "todo",
            Self::Other => "other",
        }
    }
}

/// How soon an agent needs a person: blocked, then done, then idle, then
/// working, then unknown.
pub(super) fn attention_rank(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::Blocked => 0,
        AgentStatus::Done => 1,
        AgentStatus::Idle => 2,
        AgentStatus::Working => 3,
        AgentStatus::Unknown => 4,
    }
}

/// Folder siblings in the order arranged by hand, the rest after them by name.
fn sort_siblings(siblings: &mut [String], order: &[String]) {
    let rank = |path: &str| order.iter().position(|ordered| ordered == path);
    siblings.sort_by(|left, right| match (rank(left), rank(right)) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => {
            let (left, right) = (folder_name(left), folder_name(right));
            left.to_lowercase()
                .cmp(&right.to_lowercase())
                .then_with(|| left.cmp(right))
        }
    });
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
    waiting: usize,
    working: usize,
    waiting_status: Option<AgentStatus>,
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
    order: &[String],
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
        sort_siblings(siblings, order);
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
            match AgentGroup::of(item.status, item.todo) {
                AgentGroup::Waiting => {
                    stat.waiting += 1;
                    if stat
                        .waiting_status
                        .is_none_or(|status| attention_rank(item.status) < attention_rank(status))
                    {
                        stat.waiting_status = Some(item.status);
                    }
                }
                AgentGroup::Working => stat.working += 1,
                AgentGroup::Todo | AgentGroup::Other => {}
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
            waiting: stats.map_or(0, |stats| stats.waiting),
            working: stats.map_or(0, |stats| stats.working),
            waiting_status: stats
                .and_then(|stats| stats.waiting_status)
                .unwrap_or(AgentStatus::Idle),
            muted: false,
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
        &folders.order,
        filter.is_active(),
    )
    .into_iter()
    .flat_map(|row| match row {
        TreeRow::Folder(folder) => vec![SpacesRow::Folder(folders.mark_muted(folder))],
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
    // Agents arranged by hand come first in their folder, in that order.
    let mut order = order.iter().collect::<Vec<_>>();
    if !folders.agent_order.is_empty() {
        let rank = |pane_id: &String| {
            folders
                .agent_order
                .iter()
                .position(|ordered| ordered == pane_id)
                .unwrap_or(usize::MAX)
        };
        order.sort_by_key(|pane_id| rank(pane_id));
    }
    let items = order
        .into_iter()
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
        &folders.order,
        filter.is_active(),
    )
    .into_iter()
    .map(|row| match row {
        TreeRow::Folder(folder) => AgentListRow::Folder(folders.mark_muted(folder)),
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
            AgentListRow::Folder(_) | AgentListRow::Group { .. } => None,
        })
        .collect()
}

/// Every agent by how soon it needs a person: waiting (blocked, done, idle),
/// working, marked TODO, unknown; the latest change first within each state.
pub(super) fn priority_agent_order(snapshot: &ClientShellSnapshot) -> Vec<String> {
    let mut agents = snapshot.agents.iter().collect::<Vec<_>>();
    agents.sort_by_key(|agent| {
        (
            AgentGroup::of(agent.agent_status, agent_is_todo(agent)),
            attention_rank(agent.agent_status),
            std::cmp::Reverse(agent.state_change_seq),
        )
    });
    agents
        .into_iter()
        .map(|agent| agent.pane_id.clone())
        .collect()
}

/// The agents waiting for a person, most urgent first: blocked, done, idle;
/// agents marked TODO are left out. What "next waiting agent" walks.
pub(super) fn waiting_agent_order(snapshot: &ClientShellSnapshot) -> Vec<String> {
    let mut agents = snapshot
        .agents
        .iter()
        .filter(|agent| {
            AgentGroup::of(agent.agent_status, agent_is_todo(agent)) == AgentGroup::Waiting
        })
        .collect::<Vec<_>>();
    agents.sort_by_key(|agent| {
        (
            attention_rank(agent.agent_status),
            std::cmp::Reverse(agent.state_change_seq),
        )
    });
    agents
        .into_iter()
        .map(|agent| agent.pane_id.clone())
        .collect()
}

/// The agents list of a sidebar with folders: the folder tree (agents in
/// their spaces' order inside each folder), or, sorted by priority, everyone
/// in the groups waiting, working and other.
pub(super) fn agent_panel_rows(
    snapshot: &ClientShellSnapshot,
    folders: &SidebarFolders,
    by_priority: bool,
    filter: &SidebarFilter,
) -> Vec<AgentListRow> {
    if !by_priority {
        let order = super::agent_sidebar::ordered_agent_pane_ids(
            snapshot,
            crate::config::AgentPanelSortConfig::Spaces,
        );
        return agent_list_rows(snapshot, &order, folders, filter);
    }
    let agents = priority_agent_order(snapshot)
        .into_iter()
        .filter_map(|pane_id| {
            let agent = snapshot
                .agents
                .iter()
                .find(|agent| agent.pane_id == pane_id)?;
            let folder = folders.agent_folder(&agent.pane_id, &agent.workspace_id);
            (!filter.is_active() || filter.matches(&agent_text(snapshot, agent, folder))).then(
                || {
                    (
                        pane_id,
                        AgentGroup::of(agent.agent_status, agent_is_todo(agent)),
                        agent.agent_status,
                    )
                },
            )
        })
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    for (index, (pane_id, group, status)) in agents.iter().enumerate() {
        let collapsed = folders.collapsed_groups.contains(group.label());
        if index == 0 || agents[index - 1].1 != *group {
            rows.push(AgentListRow::Group {
                group: *group,
                count: agents
                    .iter()
                    .filter(|(_, candidate, _)| candidate == group)
                    .count(),
                status: *status,
                collapsed,
            });
        }
        if !collapsed {
            rows.push(AgentListRow::Agent {
                pane_id: pane_id.clone(),
                depth: 0,
            });
        }
    }
    rows
}

/// Every agent in the order the agents list shows it, collapsed folders
/// included: the order agent numbers and next/previous agent follow.
pub(super) fn agent_panel_order(
    snapshot: &ClientShellSnapshot,
    folders: &SidebarFolders,
    by_priority: bool,
) -> Vec<String> {
    if by_priority {
        return priority_agent_order(snapshot);
    }
    let order = super::agent_sidebar::ordered_agent_pane_ids(
        snapshot,
        crate::config::AgentPanelSortConfig::Spaces,
    );
    agent_order_in_folders(snapshot, order, folders)
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

/// One line of the folder picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum FolderChoice {
    TopLevel,
    Folder(String),
    /// A folder named by what was typed, made on the spot.
    New(String),
}

impl FolderChoice {
    pub(super) fn folder(&self) -> Option<&str> {
        match self {
            Self::TopLevel => None,
            Self::Folder(path) | Self::New(path) => Some(path),
        }
    }
}

/// Folders in tree order: each folder followed by its subfolders, siblings
/// in the sidebar's order.
pub(super) fn folders_in_tree_order(paths: BTreeSet<String>, order: &[String]) -> Vec<String> {
    let mut children = HashMap::<Option<&str>, Vec<String>>::new();
    for path in &paths {
        children
            .entry(folder_parent(path))
            .or_default()
            .push(path.clone());
    }
    for siblings in children.values_mut() {
        sort_siblings(siblings, order);
    }
    fn walk(
        children: &HashMap<Option<&str>, Vec<String>>,
        parent: Option<&str>,
        out: &mut Vec<String>,
    ) {
        for child in children.get(&parent).into_iter().flatten() {
            out.push(child.clone());
            walk(children, Some(child), out);
        }
    }
    let mut out = Vec::new();
    walk(&children, None, &mut out);
    out
}

/// What the folder picker offers for the typed words: the top level and every
/// folder while nothing is typed, else the folders holding every word, and a
/// new folder named by the words unless one has that name already.
pub(super) fn folder_choices(folders: &[String], query: &str) -> Vec<FolderChoice> {
    let filter = SidebarFilter::new(query);
    let mut choices = Vec::new();
    if !filter.is_active() {
        choices.push(FolderChoice::TopLevel);
    }
    choices.extend(
        folders
            .iter()
            .filter(|path| filter.matches(path))
            .map(|path| FolderChoice::Folder(path.clone())),
    );
    if let Some(path) = normalize_folder_path(query) {
        if !folders
            .iter()
            .any(|folder| folder.eq_ignore_ascii_case(&path))
        {
            choices.push(FolderChoice::New(path));
        }
    }
    choices
}

/// The highlighted line: the one picked with the arrows while it is still
/// offered, else the first.
pub(super) fn selected_choice_index(
    choices: &[FolderChoice],
    selected: Option<&FolderChoice>,
) -> usize {
    selected
        .and_then(|selected| choices.iter().position(|choice| choice == selected))
        .unwrap_or(0)
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
    fn picker_offers_folders_and_a_new_one() {
        let folders = folders_in_tree_order(
            ["home", "ASOUP/THEX", "ASOUP", "ASOUP/core"]
                .map(String::from)
                .into(),
            &[],
        );
        assert_eq!(folders, ["ASOUP", "ASOUP/core", "ASOUP/THEX", "home"]);
        assert_eq!(
            folder_choices(&folders, ""),
            [
                FolderChoice::TopLevel,
                FolderChoice::Folder("ASOUP".into()),
                FolderChoice::Folder("ASOUP/core".into()),
                FolderChoice::Folder("ASOUP/THEX".into()),
                FolderChoice::Folder("home".into()),
            ]
        );
        assert_eq!(
            folder_choices(&folders, "thex"),
            [
                FolderChoice::Folder("ASOUP/THEX".into()),
                FolderChoice::New("thex".into()),
            ]
        );
        assert_eq!(
            folder_choices(&folders, "asoup/thex"),
            [FolderChoice::Folder("ASOUP/THEX".into())]
        );
        let choices = folder_choices(&folders, "o");
        assert_eq!(
            selected_choice_index(&choices, Some(&FolderChoice::Folder("home".into()))),
            3
        );
        assert_eq!(
            selected_choice_index(&choices, Some(&FolderChoice::TopLevel)),
            0
        );
    }

    #[test]
    fn folders_keep_the_order_arranged_by_hand() {
        let mut folders = SidebarFolders::default();
        for path in ["ASOUP", "ASOUP/THEX", "ASOUP/core", "home", "later"] {
            folders.create(path);
        }
        assert_eq!(folders.sibling_folders(None), ["ASOUP", "home", "later"]);
        assert!(folders.place_folder("later", "ASOUP", false));
        assert_eq!(folders.sibling_folders(None), ["later", "ASOUP", "home"]);
        assert!(folders.place_folder("later", "home", true));
        assert_eq!(folders.sibling_folders(None), ["ASOUP", "home", "later"]);
        assert!(folders.shift_folder("ASOUP/THEX", -1));
        assert_eq!(
            folders_in_tree_order(folders.all_paths(), &folders.order),
            ["ASOUP", "ASOUP/THEX", "ASOUP/core", "home", "later"]
        );
        // A rename keeps the place; a move into another folder starts last.
        assert!(folders.rename("home", "Home"));
        assert_eq!(folders.sibling_folders(None), ["ASOUP", "Home", "later"]);
        assert!(folders.move_folder_into("later", Some("ASOUP")));
        assert_eq!(
            folders.sibling_folders(Some("ASOUP")),
            ["ASOUP/THEX", "ASOUP/core", "ASOUP/later"]
        );
        folders.delete("ASOUP/THEX");
        assert!(!folders.order.iter().any(|path| path == "ASOUP/THEX"));
        // Not siblings: nothing to arrange.
        assert!(!folders.place_folder("Home", "ASOUP/core", false));
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
        let rows = build_tree(
            &items,
            ["Empty".to_owned()].into(),
            &BTreeSet::new(),
            &[],
            false,
        );
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
        let rows = build_tree(&items, BTreeSet::new(), &collapsed, &[], false);
        assert_eq!(outline(&rows), ["+ASOUP (3)", "  thex-b"]);

        let rows = build_tree(&items, BTreeSet::new(), &collapsed, &[], true);
        assert_eq!(outline(&rows).len(), 5);
    }
}
