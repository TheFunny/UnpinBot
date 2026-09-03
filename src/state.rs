//! Enabled-chat persistence: a JSON file rewritten atomically on change.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use teloxide::types::ChatId;

#[derive(Serialize, Deserialize, Default)]
struct StateFile {
    enabled_chats: HashSet<i64>,
}

/// The set of chats with auto-unpin enabled, backed by `state_path`.
pub struct EnabledChats {
    chats: HashSet<ChatId>,
    path: PathBuf,
}

impl EnabledChats {
    /// Loads state from `path`. A missing file means a fresh install with an
    /// empty set; a present-but-corrupt file is a hard error (silently
    /// starting empty would disable every chat without notice).
    pub fn load(path: &Path) -> Result<Self, String> {
        let chats = match fs::read(path) {
            Ok(bytes) => {
                let file: StateFile = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
                file.enabled_chats.into_iter().map(ChatId).collect()
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        Ok(Self {
            chats,
            path: path.to_path_buf(),
        })
    }

    pub fn contains(&self, id: ChatId) -> bool {
        self.chats.contains(&id)
    }

    /// Adds `id`; returns whether it was newly added.
    pub fn insert(&mut self, id: ChatId) -> bool {
        self.chats.insert(id)
    }

    /// Removes `id`; returns whether it was present.
    pub fn remove(&mut self, id: ChatId) -> bool {
        self.chats.remove(&id)
    }

    /// Moves an enabled chat id (group migrated to supergroup).
    /// Returns false (and changes nothing) if `old` was not enabled.
    pub fn replace(&mut self, old: ChatId, new: ChatId) -> bool {
        if self.chats.remove(&old) {
            self.chats.insert(new)
        } else {
            false
        }
    }

    /// Atomically rewrites the state file: write sibling `.tmp`, then rename.
    pub fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let file = StateFile {
            enabled_chats: self.chats.iter().map(|c| c.0).collect(),
        };
        let bytes =
            serde_json::to_vec_pretty(&file).map_err(|e| format!("cannot serialize state: {e}"))?;
        fs::write(&tmp, bytes).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        fs::rename(&tmp, &self.path)
            .map_err(|e| format!("cannot rename into {}: {e}", self.path.display()))
    }
}

/// Shared handle for handler dependencies.
#[derive(Clone)]
pub struct AppState(Arc<Mutex<EnabledChats>>);

impl AppState {
    pub fn new(chats: EnabledChats) -> Self {
        Self(Arc::new(Mutex::new(chats)))
    }

    pub fn contains(&self, id: ChatId) -> bool {
        self.0.lock().expect("state poisoned").contains(id)
    }

    /// Inserts and saves atomically; rolls the insert back when saving fails.
    pub fn insert_and_save(&self, id: ChatId) -> Result<bool, String> {
        let mut guard = self.0.lock().expect("state poisoned");
        if !guard.insert(id) {
            return Ok(false);
        }
        if let Err(e) = guard.save() {
            guard.remove(id);
            return Err(e);
        }
        Ok(true)
    }

    /// Removes and saves atomically; rolls the removal back when saving fails.
    pub fn remove_and_save(&self, id: ChatId) -> Result<bool, String> {
        let mut guard = self.0.lock().expect("state poisoned");
        if !guard.remove(id) {
            return Ok(false);
        }
        if let Err(e) = guard.save() {
            guard.insert(id);
            return Err(e);
        }
        Ok(true)
    }

    pub fn replace_and_save(&self, old: ChatId, new: ChatId) -> Result<bool, String> {
        let mut guard = self.0.lock().expect("state poisoned");
        if !guard.replace(old, new) {
            return Ok(false);
        }
        if let Err(e) = guard.save() {
            guard.replace(new, old);
            return Err(e);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("state.json")
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let state = EnabledChats::load(&temp_path(&dir)).unwrap();
        assert!(!state.contains(ChatId(1)));
    }

    #[test]
    fn corrupt_file_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        fs::write(&path, "{").unwrap();
        assert!(EnabledChats::load(&path).is_err());
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut state = EnabledChats::load(&path).unwrap();
        assert!(state.insert(ChatId(-100123)));
        assert!(state.insert(ChatId(42)));
        state.save().unwrap();

        let reloaded = EnabledChats::load(&path).unwrap();
        assert!(reloaded.contains(ChatId(-100123)));
        assert!(reloaded.contains(ChatId(42)));
        assert!(!reloaded.contains(ChatId(7)));
    }

    #[test]
    fn insert_remove_replace_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = EnabledChats::load(&temp_path(&dir)).unwrap();

        assert!(state.insert(ChatId(1)));
        assert!(!state.insert(ChatId(1))); // second insert reports no change

        assert!(state.replace(ChatId(1), ChatId(2)));
        assert!(!state.contains(ChatId(1)));
        assert!(state.contains(ChatId(2)));
        assert!(!state.replace(ChatId(1), ChatId(3))); // old id absent

        assert!(state.remove(ChatId(2)));
        assert!(!state.remove(ChatId(2)));
    }

    #[test]
    fn save_creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/state.json");
        let mut state = EnabledChats::load(&path).unwrap();
        state.insert(ChatId(5));
        state.save().unwrap();
        assert!(EnabledChats::load(&path).unwrap().contains(ChatId(5)));
    }

    #[test]
    fn appstate_rolls_back_failed_save() {
        // A file where a directory is needed makes create_dir_all fail, which
        // simulates an unwritable state path and exercises the rollback path.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "x").unwrap();
        let bad_path = blocker.join("deep/state.json");
        let app = AppState::new(EnabledChats::load(&bad_path).unwrap());
        assert!(app.insert_and_save(ChatId(9)).is_err());
        // Rollback: in-memory set must not contain the chat.
        assert!(!app.contains(ChatId(9)));
    }
}
