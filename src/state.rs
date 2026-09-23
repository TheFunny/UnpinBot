//! Enabled-chat persistence: a JSON file rewritten atomically on change.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use teloxide::types::ChatId;

#[derive(Serialize, Deserialize, Default)]
struct StateFile {
    /// Defaulted so a hand-written `{}` loads as an empty set instead of
    /// hard-failing startup on a "corrupt" file.
    #[serde(default)]
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
            // A path component is a file, so `save()` can still create the
            // real directories later; treat "not a directory" like "missing".
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    || e.kind() == std::io::ErrorKind::NotADirectory =>
            {
                HashSet::new()
            }
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

    pub fn len(&self) -> usize {
        self.chats.len()
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
    /// Returns false (and changes nothing) if `old` was not enabled. `new`
    /// already being enabled is still a change: the old id left the set.
    pub fn replace(&mut self, old: ChatId, new: ChatId) -> bool {
        if self.chats.remove(&old) {
            self.chats.insert(new);
            true
        } else {
            false
        }
    }

    /// Atomically rewrites the state file: write sibling `.tmp`, flush it to
    /// disk, then rename over the target.
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
        // `sync_all` before the rename: the rename only guarantees that a
        // reader sees one whole version or the other, not that the bytes have
        // reached the disk. Without the flush a crash right after this could
        // leave an empty file, which the next start then refuses to load.
        // ponytail: the file is synced, the directory entry is not — a lost
        // rename merely reverts to the previous state, which is fine here.
        // `create_new` (O_EXCL): the fixed tmp name must not follow a planted
        // symlink; a stale tmp from an interrupted save is cleared first.
        let _ = fs::remove_file(&tmp);
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .and_then(|mut file| {
                // Owner-only: the state names every group using the bot.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(fs::Permissions::from_mode(0o600))?;
                }
                file.write_all(&bytes)?;
                file.sync_all()
            })
            .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        fs::rename(&tmp, &self.path)
            .map_err(|e| format!("cannot rename into {}: {e}", self.path.display()))?;
        log::debug!(
            "saved {} enabled chats to {}",
            self.chats.len(),
            self.path.display()
        );
        Ok(())
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

    /// Rewrites the state file once, to surface an unwritable path at startup
    /// while the operator is still watching the logs: `load` succeeds on a
    /// read-only file, and the failure would otherwise only show up as a
    /// generic "try again later" reply to the first `/enable`. Unpinning keeps
    /// working for the chats already loaded, so callers warn, not abort.
    pub fn verify_writable(&self) -> Result<(), String> {
        self.0.lock().expect("state poisoned").save()
    }

    /// Moves `old` to `new` and persists. Returns false (and changes
    /// nothing) when `old` was not enabled.
    ///
    /// A failed save does NOT roll back, unlike insert/remove: after a
    /// migration the old id is dead, and gating future updates on it would
    /// silence auto-unpin for that chat forever. The move stays in memory
    /// until the next successful save (any later state change) persists it.
    pub fn replace_and_save(&self, old: ChatId, new: ChatId) -> Result<bool, String> {
        let mut guard = self.0.lock().expect("state poisoned");
        if !guard.replace(old, new) {
            return Ok(false);
        }
        guard.save()?;
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
    fn empty_object_loads_empty() {
        // A hand-written `{}` is not corruption: only malformed JSON is.
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        fs::write(&path, "{}").unwrap();
        assert_eq!(EnabledChats::load(&path).unwrap().len(), 0);
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
        let app = AppState::new(EnabledChats::load(&bad_path).expect("ENOTDIR loads as empty"));
        assert!(app.insert_and_save(ChatId(9)).is_err());
        // Rollback: in-memory set must not contain the chat.
        assert!(!app.contains(ChatId(9)));
    }

    #[test]
    fn replace_reports_a_change_when_only_the_old_id_leaves() {
        // {old, new} both enabled — a racing /enable on the upgraded id —
        // the old id leaving IS the change, and it must reach the disk.
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        fs::write(&path, r#"{"enabled_chats":[1,2]}"#).unwrap();
        let mut state = EnabledChats::load(&path).unwrap();
        assert!(state.replace(ChatId(1), ChatId(2)));
        assert!(!state.contains(ChatId(1)));
        assert!(state.contains(ChatId(2)));
        state.save().unwrap();
        let reloaded = EnabledChats::load(&path).unwrap();
        assert!(!reloaded.contains(ChatId(1)));
        assert!(reloaded.contains(ChatId(2)));
    }

    #[test]
    fn replace_and_save_keeps_the_new_id_when_save_fails() {
        // Migration is a world-state sync: rolling back to the dead id would
        // gate every future update of the migrated chat out of auto_unpin.
        // Same unwritable-path trick as above; the set starts as {1}.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "x").unwrap();
        let bad_path = blocker.join("deep/state.json");
        let mut chats = EnabledChats::load(&bad_path).expect("ENOTDIR loads as empty");
        assert!(chats.insert(ChatId(1)));
        let app = AppState::new(chats);
        assert!(app.replace_and_save(ChatId(1), ChatId(2)).is_err());
        assert!(!app.contains(ChatId(1)), "the dead id must not return");
        assert!(
            app.contains(ChatId(2)),
            "the live id survives a failed save"
        );
    }

    #[cfg(unix)]
    #[test]
    fn saved_state_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut state = EnabledChats::load(&path).unwrap();
        state.insert(ChatId(1));
        state.save().unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
