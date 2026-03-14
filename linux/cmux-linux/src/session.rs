use crate::state::{AppState, PersistentStateSnapshot};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_SESSION_FILENAME: &str = "cmux-linux-session.json";

pub fn configured_session_path() -> PathBuf {
    env::var_os("CMUX_LINUX_SESSION_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(default_session_path)
}

pub fn load_state(path: &Path) -> Result<Option<AppState>, String> {
    if !path.exists() {
        return Ok(None);
    }

    let encoded =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let snapshot: PersistentStateSnapshot = serde_json::from_str(&encoded)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    AppState::from_persistent_snapshot(snapshot).map(Some)
}

pub fn save_state(path: &Path, state: &AppState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }

    let tmp_path = path.with_extension("tmp");
    let encoded = serde_json::to_vec_pretty(&state.to_persistent_snapshot())
        .map_err(|error| format!("encode {}: {error}", path.display()))?;
    fs::write(&tmp_path, encoded)
        .map_err(|error| format!("write {}: {error}", tmp_path.display()))?;
    fs::rename(&tmp_path, path).map_err(|error| {
        format!(
            "rename {} -> {}: {error}",
            tmp_path.display(),
            path.display()
        )
    })
}

fn default_session_path() -> PathBuf {
    let base_dir = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME").map(|home| {
                let mut path = PathBuf::from(home);
                path.push(".local/state");
                path
            })
        })
        .unwrap_or_else(|| PathBuf::from("/tmp"));

    base_dir.join("cmux").join(DEFAULT_SESSION_FILENAME)
}
