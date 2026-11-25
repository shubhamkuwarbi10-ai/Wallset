//! Where wallSet keeps things, and the tiny amount of I/O the daemon does.

use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use wallset_core::Config;

fn env_dir(var: &str) -> PathBuf {
    std::env::var_os(var).map_or_else(std::env::temp_dir, PathBuf::from).join("wallSet")
}

/// `%LOCALAPPDATA%\wallSet` — machine-local: logs and freeze frames.
#[must_use]
pub fn local_dir() -> PathBuf {
    env_dir("LOCALAPPDATA")
}

#[must_use]
pub fn freeze_frame(monitor_id: &str) -> PathBuf {
    // FNV-1a: stable, dependency-free, and file-name safe.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in monitor_id.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    local_dir().join("frames").join(format!("{h:016x}.png"))
}

/// Load the config, falling back to defaults on a missing or corrupt file.
/// A corrupt file is kept aside rather than overwritten, so a hand edit with
/// a typo is not silently lost.
pub fn load_config() -> Config {
    let (cfg, err) = Config::load_from(&wallset_core::config::default_path());
    if let Some(e) = err {
        log(&format!("config unreadable ({e}); using defaults, original kept as config.json.bad"));
    }
    cfg
}

pub fn ensure_dirs() {
    let _ = std::fs::create_dir_all(local_dir().join("frames"));
    if let Some(p) = wallset_core::config::default_path().parent() {
        let _ = std::fs::create_dir_all(p);
    }
}

/// Append one line to the daemon log. Capped at 256 KiB by truncating at
/// startup, so a long-running daemon cannot fill the disk.
pub fn log(msg: &str) {
    let path = local_dir().join("daemon.log");
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{secs}] {msg}");
    }
}

pub fn rotate_log() {
    let path = local_dir().join("daemon.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 256 * 1024) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
}

/// Register or unregister the daemon under the per-user Run key.
pub fn sync_autostart(enabled: bool) {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW};
    use windows::core::w;
    let key = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
    let name = w!("wallSet");
    // SAFETY: registry calls with NUL-terminated strings and a sized buffer.
    unsafe {
        if enabled {
            let Ok(exe) = std::env::current_exe() else { return };
            let value: Vec<u16> =
                format!("\"{}\"", exe.display()).encode_utf16().chain(std::iter::once(0)).collect();
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key,
                name,
                REG_SZ.0,
                Some(value.as_ptr().cast()),
                (value.len() * 2) as u32,
            );
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, key, name);
        }
    }
}
