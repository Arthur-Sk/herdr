use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Written by every build that keeps the keys it does not know. A file without
/// it was last written by an older build, which rewrote it with its own keys only.
const FORMAT_KEY: &str = "layout_format";
const FORMAT: u64 = 1;

/// Every key the builds before FORMAT could write, with the generation that
/// added it: 0 upstream herdr, 1 fork folders (fd9bb257, 52066519), 2 folder
/// order and filter (9f3e2ca6), 3 mute, folded groups and agent order
/// (9241b6d3). The list is closed: later keys are written only by builds that
/// keep unknown keys, so an older build drops them all.
const LEGACY_KEYS: &[(&str, u8)] = &[
    ("sidebar_width", 0),
    ("sidebar_section_split", 0),
    ("sidebar_collapsed", 0),
    ("agent_panel_sort", 0),
    ("collapsed_groups", 0),
    ("remote_collapsed_groups", 0),
    ("folders", 1),
    ("folders.folders", 1),
    ("folders.spaces", 1),
    ("folders.agents", 1),
    ("folders.collapsed_spaces", 1),
    ("folders.collapsed_agents", 1),
    ("folders.order", 2),
    ("folders.filter", 2),
    ("folders.muted", 3),
    ("folders.collapsed_groups", 3),
    ("folders.agent_order", 3),
];

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub(super) struct ClientRemoteCollapsedGroups {
    pub(super) profile_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) collapsed_groups: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub(super) struct ClientChromePreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) sidebar_width: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) sidebar_section_split: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) sidebar_collapsed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) agent_panel_sort: Option<crate::config::AgentPanelSortConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) collapsed_groups: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) remote_collapsed_groups: Vec<ClientRemoteCollapsedGroups>,
    #[serde(
        default,
        skip_serializing_if = "super::folders::SidebarFolders::is_empty"
    )]
    pub(super) folders: super::folders::SidebarFolders,
    /// Keys a newer build wrote, kept as they are when this build saves.
    #[serde(flatten)]
    pub(super) unknown: Map<String, Value>,
}

pub(super) fn path_for_local_endpoint(socket_path: &Path) -> PathBuf {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in socket_path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    crate::config::state_dir()
        .join("client-shell")
        .join(format!("local-{hash:016x}.json"))
}

/// SSH preferences belong to the target/session, not its per-process bridge socket.
pub(super) fn path_for_remote_endpoint(target: &str, session: &str) -> PathBuf {
    use sha2::{Digest as _, Sha256};

    let mut hash = Sha256::new();
    for part in [target, session] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    crate::config::state_dir()
        .join("client-shell")
        .join(format!("remote-{:x}.json", hash.finalize()))
}

pub(super) fn load(path: &Path) -> Option<ClientChromePreferences> {
    let mut saved = read_json(path)?;
    let written_by_current_build = saved.as_object_mut()?.remove(FORMAT_KEY).is_some();
    if !written_by_current_build {
        if let Some(full) = read_json(&full_copy_path(path)) {
            restore_dropped_keys(&mut saved, &full);
        }
    }
    serde_json::from_value(saved).ok()
}

/// Saves `preferences` over every key this build knows, as a fresh window would.
#[cfg(test)]
pub(super) fn store(path: &Path, mut preferences: ClientChromePreferences) -> Result<(), String> {
    let _lock = lock(path);
    // Keys this build does not know came from a newer one: keep them as the file
    // has them now, so going back to an older build loses nothing.
    if let Some(saved) = load(path) {
        preferences.unknown = saved.unknown;
        preferences.folders.unknown = saved.folders.unknown;
    }
    write_layout(path, &preferences)
}

/// Saves a window's layout when other windows of the same endpoint may have
/// saved theirs since: only what this window changed since `base` (its layout
/// at its last save or reload) goes over the file as it stands now. Returns the
/// layout saved.
pub(super) fn store_merged(
    path: &Path,
    base: &ClientChromePreferences,
    ours: ClientChromePreferences,
) -> Result<ClientChromePreferences, String> {
    let _lock = lock(path);
    let merged = match load(path) {
        Some(theirs) => merge(base, ours, theirs),
        None => ours,
    };
    write_layout(path, &merged)?;
    Ok(merged)
}

/// `ours`'s changes since `base`, laid over `theirs`. Lists of names (folds,
/// mutes, folders) and per-id maps merge entry by entry, so two windows that
/// changed different entries keep both; a value both changed takes `ours`.
pub(super) fn merge(
    base: &ClientChromePreferences,
    ours: ClientChromePreferences,
    theirs: ClientChromePreferences,
) -> ClientChromePreferences {
    ClientChromePreferences {
        sidebar_width: pick(
            &base.sidebar_width,
            ours.sidebar_width,
            theirs.sidebar_width,
        ),
        sidebar_section_split: pick(
            &base.sidebar_section_split,
            ours.sidebar_section_split,
            theirs.sidebar_section_split,
        ),
        sidebar_collapsed: pick(
            &base.sidebar_collapsed,
            ours.sidebar_collapsed,
            theirs.sidebar_collapsed,
        ),
        agent_panel_sort: pick(
            &base.agent_panel_sort,
            ours.agent_panel_sort,
            theirs.agent_panel_sort,
        ),
        collapsed_groups: super::folders::merge_set(
            &base.collapsed_groups.iter().cloned().collect(),
            ours.collapsed_groups.into_iter().collect(),
            theirs.collapsed_groups.into_iter().collect(),
        )
        .into_iter()
        .collect(),
        remote_collapsed_groups: pick(
            &base.remote_collapsed_groups,
            ours.remote_collapsed_groups,
            theirs.remote_collapsed_groups,
        ),
        folders: super::folders::SidebarFolders::merged(
            &base.folders,
            ours.folders,
            theirs.folders,
        ),
        unknown: theirs.unknown,
    }
}

/// `ours` if this window changed the value since `base`, else `theirs`.
pub(super) fn pick<T: PartialEq>(base: &T, ours: T, theirs: T) -> T {
    if ours != *base {
        ours
    } else {
        theirs
    }
}

/// What tells a rewrite of the layout file from the last one seen: every save
/// replaces the file, so its inode changes too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FileStamp {
    modified: Option<std::time::SystemTime>,
    len: u64,
    inode: u64,
}

pub(super) fn stamp(path: &Path) -> Option<FileStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let inode = std::os::unix::fs::MetadataExt::ino(&metadata);
    #[cfg(not(unix))]
    let inode = 0;
    Some(FileStamp {
        modified: metadata.modified().ok(),
        len: metadata.len(),
        inode,
    })
}

/// Windows of one endpoint take turns reading, merging and writing the file.
fn lock(path: &Path) -> Option<std::fs::File> {
    let lock_path = sibling_path(path, ".lock");
    std::fs::create_dir_all(lock_path.parent()?).ok()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)
        .ok()?;
    file.lock().ok()?;
    Some(file)
}

fn write_layout(path: &Path, preferences: &ClientChromePreferences) -> Result<(), String> {
    let mut value = serde_json::to_value(preferences)
        .map_err(|error| format!("failed to encode client shell state: {error}"))?;
    if let Some(object) = value.as_object_mut() {
        object.insert(FORMAT_KEY.to_owned(), FORMAT.into());
    }
    let content = serde_json::to_vec_pretty(&value)
        .map_err(|error| format!("failed to encode client shell state: {error}"))?;
    replace_file(path, &content)?;
    // Builds older than FORMAT never touch this copy; `load` takes back from it
    // what they dropped.
    replace_file(&full_copy_path(path), &content)
}

fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Removes a layout file with the copy and lock kept beside it.
#[cfg(test)]
pub(super) fn remove_layout_files(path: &Path) {
    for file in [
        path.to_path_buf(),
        full_copy_path(path),
        sibling_path(path, ".lock"),
    ] {
        let _ = std::fs::remove_file(file);
    }
}

/// The last layout a current build saved, beside the file itself.
fn full_copy_path(path: &Path) -> PathBuf {
    sibling_path(path, ".full")
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn legacy_generation(key: &str) -> Option<u8> {
    LEGACY_KEYS
        .iter()
        .find(|(known, _)| *known == key)
        .map(|(_, generation)| *generation)
}

/// The generation of the build that wrote `saved`, judged by the newest key in
/// it. A key that build knew but left empty is skipped when saved, so an older
/// build can only be mistaken for an even older one.
fn written_generation(saved: &Map<String, Value>) -> u8 {
    let folder_keys = saved
        .get("folders")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|folders| folders.keys().map(|key| format!("folders.{key}")));
    saved
        .keys()
        .cloned()
        .chain(folder_keys)
        .filter_map(|key| legacy_generation(&key))
        .max()
        .unwrap_or(0)
}

/// An older build rewrote `saved` with only the keys it knew. Brings back from
/// the full copy each key that build could not have written; the keys it knew
/// stay as it left them, cleared ones included.
fn restore_dropped_keys(saved: &mut Value, full: &Value) {
    let (Some(saved), Some(full)) = (saved.as_object_mut(), full.as_object()) else {
        return;
    };
    let writer = written_generation(saved);
    let dropped = |key: &str| legacy_generation(key).is_none_or(|generation| generation > writer);
    for (key, value) in full {
        if key == FORMAT_KEY {
            continue;
        }
        match saved.get_mut(key) {
            None if dropped(key) => {
                saved.insert(key.clone(), value.clone());
            }
            Some(Value::Object(folders)) if key == "folders" => {
                for (folder_key, folder_value) in value.as_object().into_iter().flatten() {
                    if !folders.contains_key(folder_key)
                        && dropped(&format!("folders.{folder_key}"))
                    {
                        folders.insert(folder_key.clone(), folder_value.clone());
                    }
                }
            }
            _ => {}
        }
    }
}

fn replace_file(path: &Path, content: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("invalid client shell state path: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create client shell state directory: {error}"))?;
    let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    let mut temp_name = path
        .file_name()
        .ok_or_else(|| format!("invalid client shell state path: {}", path.display()))?
        .to_os_string();
    temp_name.push(format!(".tmp-{}-{sequence}", std::process::id()));
    let temp_path = parent.join(temp_name);
    std::fs::write(&temp_path, content)
        .map_err(|error| format!("failed to write client shell state: {error}"))?;
    std::fs::rename(&temp_path, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp_path);
        format!("failed to replace client shell state: {error}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_paths_are_stable_and_distinct() {
        let first = path_for_local_endpoint(Path::new("/run/herdr/one.sock"));
        let again = path_for_local_endpoint(Path::new("/run/herdr/one.sock"));
        let second = path_for_local_endpoint(Path::new("/run/herdr/two.sock"));
        assert_eq!(first, again);
        assert_ne!(first, second);
    }

    #[test]
    fn remote_preferences_distinguish_targets_sessions_and_component_boundaries() {
        let original = path_for_remote_endpoint("dev@build", "agents");
        assert_eq!(original, path_for_remote_endpoint("dev@build", "agents"));
        assert_ne!(original, path_for_remote_endpoint("other@build", "agents"));
        assert_ne!(original, path_for_remote_endpoint("dev@build", "other"));
        assert_ne!(
            path_for_remote_endpoint("ab", "c"),
            path_for_remote_endpoint("a", "bc")
        );
        assert_ne!(
            original,
            path_for_local_endpoint(Path::new("dev@build/agents"))
        );
    }

    #[test]
    fn legacy_preferences_default_remote_collapses() {
        let preferences: ClientChromePreferences =
            serde_json::from_str(r#"{"collapsed_groups":["/repo"]}"#)
                .expect("legacy client chrome preferences");

        assert_eq!(preferences.collapsed_groups, ["/repo"]);
        assert!(preferences.remote_collapsed_groups.is_empty());
    }

    #[test]
    fn concurrent_stores_leave_complete_preferences() {
        let path = std::env::temp_dir().join(format!(
            "herdr-shell-concurrent-preferences-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let writers = (20..28)
            .map(|width| {
                let path = path.clone();
                std::thread::spawn(move || {
                    store(
                        &path,
                        ClientChromePreferences {
                            sidebar_width: Some(width),
                            ..ClientChromePreferences::default()
                        },
                    )
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().expect("preference writer").expect("store");
        }
        assert!(load(&path)
            .and_then(|saved| saved.sidebar_width)
            .is_some_and(|width| (20..28).contains(&width)));
        remove_layout(&path);
    }

    #[test]
    fn repeated_store_replaces_existing_preferences() {
        let path = std::env::temp_dir().join(format!(
            "herdr-shell-preferences-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        store(
            &path,
            ClientChromePreferences {
                sidebar_width: Some(24),
                ..ClientChromePreferences::default()
            },
        )
        .expect("first preference store");
        store(
            &path,
            ClientChromePreferences {
                sidebar_width: Some(32),
                ..ClientChromePreferences::default()
            },
        )
        .expect("replacement preference store");
        assert_eq!(load(&path).and_then(|saved| saved.sidebar_width), Some(32));
        remove_layout(&path);
    }

    fn layout_path(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "herdr-shell-layout-{name}-{}.json",
            std::process::id()
        ));
        remove_layout(&path);
        path
    }

    fn remove_layout(path: &Path) {
        remove_layout_files(path);
    }

    fn write_raw(path: &Path, value: serde_json::Value) {
        std::fs::write(path, serde_json::to_vec(&value).expect("json")).expect("write layout");
    }

    fn read_raw(path: &Path) -> serde_json::Value {
        read_json(path).expect("layout file")
    }

    fn arranged() -> ClientChromePreferences {
        let mut folders = super::super::folders::SidebarFolders::default();
        folders.spaces.insert("w1".into(), "A".into());
        folders.spaces.insert("w2".into(), "A".into());
        folders.order = vec!["A".into()];
        folders.filter = "thex".into();
        folders.muted.insert("A".into());
        folders.agent_order = vec!["w1:p1".into()];
        ClientChromePreferences {
            sidebar_width: Some(30),
            folders,
            ..ClientChromePreferences::default()
        }
    }

    #[test]
    fn keys_from_a_newer_build_survive_a_save() {
        let path = layout_path("newer");
        write_raw(
            &path,
            serde_json::json!({
                "layout_format": 1,
                "sidebar_width": 24,
                "pinned_agents": ["w1:p1"],
                "folders": {"spaces": {"w1": "A"}, "colors": {"A": "red"}}
            }),
        );
        let base = load(&path).expect("layout");
        assert_eq!(base.folders.space_folder("w1"), Some("A"));
        let mut ours = base.clone();
        ours.sidebar_width = Some(40);
        ours.folders.spaces.insert("w2".into(), "A".into());
        let saved = store_merged(&path, &base, ours).expect("store");
        assert_eq!(saved.unknown["pinned_agents"], serde_json::json!(["w1:p1"]));
        assert_eq!(
            saved.folders.unknown["colors"],
            serde_json::json!({"A": "red"})
        );
        assert_eq!(saved.folders.space_folder("w2"), Some("A"));
        // A save built from scratch keeps them too: they are read from the file.
        store(
            &path,
            ClientChromePreferences {
                sidebar_width: Some(41),
                ..ClientChromePreferences::default()
            },
        )
        .expect("store");

        let raw = read_raw(&path);
        assert_eq!(raw["sidebar_width"], 41);
        assert_eq!(raw["pinned_agents"], serde_json::json!(["w1:p1"]));
        assert_eq!(raw["folders"]["colors"], serde_json::json!({"A": "red"}));
        assert_eq!(raw["layout_format"], 1);
        assert_eq!(read_raw(&full_copy_path(&path)), raw);
        remove_layout(&path);
    }

    #[test]
    fn keys_an_older_build_dropped_come_back_and_its_own_changes_stay() {
        let path = layout_path("older");
        store(&path, arranged()).expect("store");
        // The folder order build (9f3e2ca6) rewrites the file: it moved w2 out,
        // cleared the filter and knew nothing of mutes and agent order.
        write_raw(
            &path,
            serde_json::json!({
                "sidebar_width": 33,
                "folders": {"spaces": {"w1": "A"}, "order": ["A"]}
            }),
        );

        let loaded = load(&path).expect("layout");
        assert_eq!(loaded.sidebar_width, Some(33));
        assert_eq!(loaded.folders.space_folder("w2"), None);
        assert_eq!(loaded.folders.filter, "");
        assert!(loaded.folders.muted.contains("A"));
        assert_eq!(loaded.folders.agent_order, ["w1:p1"]);
        remove_layout(&path);
    }

    #[test]
    fn folders_come_back_after_the_upstream_build_saved() {
        let path = layout_path("upstream");
        store(&path, arranged()).expect("store");
        write_raw(&path, serde_json::json!({"sidebar_width": 28}));

        let loaded = load(&path).expect("layout");
        assert_eq!(loaded.sidebar_width, Some(28));
        assert_eq!(loaded.folders.space_folder("w2"), Some("A"));
        assert!(loaded.folders.muted.contains("A"));
        assert_eq!(loaded.folders.filter, "thex");
        remove_layout(&path);
    }

    #[test]
    fn what_a_current_build_cleared_stays_cleared() {
        let path = layout_path("cleared");
        store(&path, arranged()).expect("store");
        let mut cleared = arranged();
        cleared.folders.muted.clear();
        cleared.folders.filter.clear();
        store(&path, cleared).expect("store");

        let loaded = load(&path).expect("layout");
        assert!(loaded.folders.muted.is_empty());
        assert_eq!(loaded.folders.filter, "");
        assert_eq!(loaded.folders.agent_order, ["w1:p1"]);
        remove_layout(&path);
    }

    #[test]
    fn two_windows_saving_in_turn_keep_each_others_changes() {
        let path = layout_path("windows");
        store(&path, arranged()).expect("store");
        let base = load(&path).expect("layout");

        // The first window files w3, mutes nothing new and widens its sidebar.
        let mut first = base.clone();
        first.folders.spaces.insert("w3".into(), "B".into());
        first.sidebar_width = Some(36);
        let saved = store_merged(&path, &base, first).expect("first save");
        assert_eq!(saved.folders.space_folder("w3"), Some("B"));

        // The second window still holds the old layout: it unmutes A, moves w1
        // out and collapses a folder. Its save keeps the first window's work.
        let mut second = base.clone();
        second.folders.muted.clear();
        second.folders.spaces.remove("w1");
        second.folders.collapsed_spaces.insert("A".into());
        let saved = store_merged(&path, &base, second).expect("second save");

        assert_eq!(saved, load(&path).expect("layout"));
        assert_eq!(saved.sidebar_width, Some(36));
        assert_eq!(saved.folders.space_folder("w3"), Some("B"));
        assert_eq!(saved.folders.space_folder("w2"), Some("A"));
        assert_eq!(saved.folders.space_folder("w1"), None);
        assert!(saved.folders.muted.is_empty());
        assert!(saved.folders.collapsed_spaces.contains("A"));
        remove_layout(&path);
    }

    #[test]
    fn hand_orders_keep_entries_another_window_added() {
        let path = layout_path("orders");
        store(&path, arranged()).expect("store");
        let base = load(&path).expect("layout");
        let mut first = base.clone();
        first.folders.order.push("B".into());
        store_merged(&path, &base, first).expect("first save");

        let mut second = base.clone();
        second.folders.order = vec!["C".into(), "A".into()];
        let saved = store_merged(&path, &base, second).expect("second save");
        assert_eq!(saved.folders.order, ["C", "A", "B"]);
        remove_layout(&path);
    }
}
