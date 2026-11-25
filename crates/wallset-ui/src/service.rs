//! Talking to the daemon: the config file plus one window message.

use std::path::PathBuf;
use wallset_core::Config;

/// Must match `wallset-daemon/src/paths.rs`.
pub fn config_file() -> PathBuf {
    std::env::var_os("APPDATA")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("wallSet")
        .join("config.json")
}

pub fn load_config() -> Config {
    std::fs::read_to_string(config_file())
        .ok()
        .and_then(|s| serde_json::from_str::<Config>(&s).ok())
        .map(|mut c| {
            c.sanitise();
            c
        })
        .unwrap_or_default()
}

/// Write atomically: a crash mid-write must never leave the daemon a
/// truncated config to choke on.
pub fn save_config(cfg: &Config) -> std::io::Result<()> {
    let path = config_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(cfg).map_err(std::io::Error::other)?)?;
    std::fs::rename(tmp, path)
}

/// Read-modify-write: load the config from disk, apply `change`, save.
/// Returns what was saved. Never writes a caller's stale snapshot, so
/// edits made elsewhere since the caller loaded its copy survive.
pub fn update_config(change: impl FnOnce(&mut Config)) -> std::io::Result<Config> {
    let mut fresh = load_config();
    change(&mut fresh);
    fresh.sanitise();
    save_config(&fresh)?;
    Ok(fresh)
}

#[cfg(windows)]
fn daemon_window() -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    use windows::core::{PCWSTR, w};
    // SAFETY: plain lookup by class name.
    unsafe { FindWindowW(w!("wallSet.Daemon"), PCWSTR::null()) }.ok()
}

#[cfg(windows)]
pub fn daemon_running() -> bool {
    daemon_window().is_some()
}

/// Tell a running daemon to reload, or start one.
#[cfg(windows)]
pub fn notify_or_start_daemon() -> Result<&'static str, String> {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
    if let Some(h) = daemon_window() {
        // SAFETY: posting WM_APP_RELOAD (WM_APP + 3) to the daemon.
        unsafe { PostMessageW(Some(h), WM_APP + 3, WPARAM(0), LPARAM(0)) }.map_err(|e| e.to_string())?;
        return Ok("Background service updated.");
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("wallset-daemon.exe");
    std::process::Command::new(&exe).spawn().map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
    Ok("Background service started.")
}

#[cfg(not(windows))]
pub fn daemon_running() -> bool {
    false
}

#[cfg(not(windows))]
pub fn notify_or_start_daemon() -> Result<&'static str, String> {
    Err("the background service is Windows-only for now".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wallset_core::{Assignment, FitMode, MonitorId};
    use wallset_core::media::MediaKind;

    #[test]
    fn saving_settings_keeps_assignments_written_by_someone_else() {
        let dir = std::env::temp_dir().join(format!("wallset-ui-test-{}", std::process::id()));
        // SAFETY (edition 2024): single-threaded test; nothing else reads env.
        unsafe { std::env::set_var("APPDATA", &dir) };

        // This window loads its snapshot: no assignments.
        let stale = load_config();
        assert!(stale.assignments.is_empty());

        // Meanwhile another writer assigns a wallpaper.
        let mut other = Config::default();
        other.assign(Assignment {
            monitor: MonitorId("M1".into()),
            path: "elsewhere.mp4".into(),
            kind: MediaKind::Video,
            fit: FitMode::Fill,
        });
        save_config(&other).unwrap();

        // This window now saves a settings change.
        let low_power = !stale.render.low_power;
        let saved = update_config(|c| c.render.low_power = low_power).unwrap();

        assert_eq!(saved.render.low_power, low_power, "our change applied");
        assert_eq!(saved.assignments.len(), 1, "the other writer's assignment survived");
        assert_eq!(load_config(), saved, "and that is what is on disk");
        let _ = std::fs::remove_dir_all(dir);
    }
}
