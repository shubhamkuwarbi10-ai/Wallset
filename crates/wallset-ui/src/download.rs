//! Wallpapers from web links: downloaded once into wallSet's own folder,
//! then used like any local file.
//!
//! Downloaded rather than streamed: a wallpaper loops for hours, a stream
//! would fetch it again on every loop and stop when offline, and Windows
//! needs a local file for a still image anyway.
//!
//! The transfer is Windows' own `curl.exe` (in System32 since Windows 10
//! 1803), so no HTTP or TLS library ships with wallSet and certificates come
//! from the Windows store.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// Largest file accepted. A 4K wallpaper video is rarely over a few hundred
/// MB; this stops a wrong link from filling the disk.
const MAX_BYTES: &str = "2G";

pub struct Download {
    pub url: String,
    child: Child,
    part: PathBuf,
    dir: PathBuf,
}

impl Download {
    pub fn start(url: &str) -> Result<Self, String> {
        Self::start_in(url, downloads_dir())
    }

    fn start_in(url: &str, dir: PathBuf) -> Result<Self, String> {
        let url = url.trim().to_string();
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("https://") || lower.starts_with("http://")) {
            return Err("Paste a link that starts with https:// or http://.".into());
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        let part = dir.join(format!("{:016x}.part", hash(&url)));
        let mut cmd = Command::new(curl());
        cmd.args([
            "--location",
            "--fail",
            "--silent",
            "--show-error",
            "--max-filesize",
            MAX_BYTES,
            "--connect-timeout",
            "20",
            // Never let a redirect reach file:// or other schemes.
            "--proto",
            "=http,https",
            "--proto-redir",
            "=http,https",
            // Some networks block revocation checks; still verify the chain.
            "--ssl-revoke-best-effort",
            "--user-agent",
            "wallSet",
            "--write-out",
            "%{content_type}",
            "--output",
        ])
        .arg(&part)
        // --url, so a link can never be read as an option.
        .arg("--url")
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let child = cmd.spawn().map_err(|e| format!("Cannot start curl.exe: {e}"))?;
        Ok(Self { url, child, part, dir })
    }

    /// Bytes received so far.
    pub fn received(&self) -> u64 {
        std::fs::metadata(&self.part).map_or(0, |m| m.len())
    }

    /// `None` while running; then the downloaded file, or why it failed.
    pub fn poll(&mut self) -> Option<Result<PathBuf, String>> {
        let status = match self.child.try_wait() {
            Ok(None) => return None,
            Ok(Some(s)) => s,
            Err(e) => return Some(Err(e.to_string())),
        };
        let read = |r: Option<&mut dyn std::io::Read>| {
            let mut s = String::new();
            if let Some(r) = r {
                let _ = r.read_to_string(&mut s);
            }
            s.trim().to_string()
        };
        let content_type = read(self.child.stdout.as_mut().map(|s| s as &mut dyn std::io::Read));
        let err = read(self.child.stderr.as_mut().map(|s| s as &mut dyn std::io::Read));
        Some(if status.success() { self.finish(&content_type) } else { Err(curl_error(status.code(), &err)) })
    }

    fn finish(&self, content_type: &str) -> Result<PathBuf, String> {
        let Some(name) = file_name(&self.url, content_type) else {
            let _ = std::fs::remove_file(&self.part);
            return Err(if content_type.starts_with("text/html") {
                "That link is a web page, not a media file. Use a direct link to an image or video \
                 (one that ends in .mp4, .jpg and so on)."
                    .into()
            } else {
                format!("That link is not an image or video (it is {content_type}).")
            });
        };
        let path = self.dir.join(name);
        std::fs::rename(&self.part, &path).map_err(|e| format!("Cannot save the download: {e}"))?;
        Ok(path)
    }
}

impl Drop for Download {
    /// Cancelled or abandoned: stop the transfer and remove the partial file.
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = std::fs::remove_file(&self.part);
    }
}

/// `%LOCALAPPDATA%\wallSet\downloads`. Removed by the uninstaller when the
/// user chooses to remove their settings.
pub fn downloads_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("wallSet")
        .join("downloads")
}

fn curl() -> PathBuf {
    std::env::var_os("WINDIR")
        .map_or_else(|| PathBuf::from("C:\\Windows"), PathBuf::from)
        .join("System32")
        .join("curl.exe")
}

fn hash(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

const MEDIA: &[&str] = &[
    "mp4", "mkv", "webm", "mov", "avi", "m4v", "wmv", "gif", "png", "jpg", "jpeg", "bmp", "webp", "tif", "tiff",
    "jxl", "avif", "heic",
];

/// A safe local name: the link's own file name where it has a media
/// extension, else one from the content type; plus a hash of the link so two
/// different `video.mp4` links do not overwrite each other. `None` when the
/// download is not media at all.
fn file_name(url: &str, content_type: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let last = path.rsplit('/').next().unwrap_or("");
    let (stem, ext) = match last.rsplit_once('.') {
        Some((s, e)) if MEDIA.contains(&e.to_ascii_lowercase().as_str()) => (s, e.to_ascii_lowercase()),
        _ => (last, ext_for(content_type)?.to_string()),
    };
    let stem: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .take(60)
        .collect();
    let stem = if stem.trim_matches('-').is_empty() { "wallpaper" } else { stem.trim_matches('-') };
    Some(format!("{stem}-{:08x}.{ext}", hash(url) as u32))
}

fn ext_for(content_type: &str) -> Option<&'static str> {
    let t = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    Some(match t.as_str() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/avif" => "avif",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        _ => return None,
    })
}

/// curl's exit codes, in words.
fn curl_error(code: Option<i32>, stderr: &str) -> String {
    match code {
        Some(6) => "Cannot find that website. Check the link and your connection.".into(),
        Some(7) | Some(28) => "Cannot reach that website (connection failed or timed out).".into(),
        Some(22) => format!("The website refused the download ({}).", stderr.rsplit(": ").next().unwrap_or(stderr)),
        Some(35) | Some(60) => "The website's secure connection could not be verified.".into(),
        Some(63) => format!("The file is larger than {MAX_BYTES}B, too big for a wallpaper."),
        _ if stderr.is_empty() => "The download failed.".into(),
        _ => format!("The download failed: {stderr}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_the_link_or_the_content_type() {
        let n = file_name("https://example.com/a/Snow%20Night.MP4?x=1", "application/octet-stream").unwrap();
        assert!(n.starts_with("Snow-20Night-") && n.ends_with(".mp4"), "{n}");
        let n = file_name("https://cdn.example.com/get?id=7", "image/jpeg; charset=binary").unwrap();
        assert!(n.starts_with("get-") && n.ends_with(".jpg"), "{n}");
        assert!(file_name("https://example.com/watch?v=1", "text/html; charset=utf-8").is_none());
    }

    #[test]
    fn names_are_safe_and_distinct() {
        let a = file_name("https://a.example/../../x.png", "").unwrap();
        assert!(!a.contains('/') && !a.contains('\\') && !a.contains(".."), "{a}");
        let b = file_name("https://b.example/x.png", "").unwrap();
        assert_ne!(a, b);
    }

    /// A one-request-per-connection HTTP server on localhost: path -> (status, content type, body).
    fn serve(routes: Vec<(&'static str, u16, &'static str, Vec<u8>)>) -> u16 {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                let _ = BufReader::new(&stream).read_line(&mut line);
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, ctype, body) = routes
                    .iter()
                    .find(|r| r.0 == path)
                    .map_or((404, "text/plain", b"not found".to_vec()), |r| (r.1, r.2, r.3.clone()));
                let mut s = stream;
                let _ = write!(s, "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(&body);
            }
        });
        port
    }

    fn run(url: &str, dir: &std::path::Path) -> Result<PathBuf, String> {
        let mut d = Download::start_in(url, dir.to_path_buf())?;
        for _ in 0..200 {
            if let Some(r) = d.poll() {
                return r;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("download did not finish");
    }

    #[test]
    fn downloads_media_and_refuses_pages_and_errors() {
        let png = b"\x89PNG fake image bytes".to_vec();
        let port = serve(vec![
            ("/get?id=7", 200, "image/png", png.clone()),
            ("/watch", 200, "text/html; charset=utf-8", b"<html></html>".to_vec()),
        ]);
        let dir = std::env::temp_dir().join(format!("wallset-dl-test-{}", std::process::id()));
        let base = format!("http://127.0.0.1:{port}");

        let saved = run(&format!("{base}/get?id=7"), &dir).expect("image downloads");
        assert_eq!(saved.extension().unwrap(), "png");
        assert_eq!(std::fs::read(&saved).unwrap(), png);

        let page = run(&format!("{base}/watch"), &dir).unwrap_err();
        assert!(page.contains("web page"), "{page}");
        let missing = run(&format!("{base}/nope.mp4"), &dir).unwrap_err();
        assert!(missing.contains("refused") && missing.contains("404"), "{missing}");

        // Only the finished image is left behind: no partial files.
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 1, "{left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_web_links_are_accepted() {
        assert!(Download::start("file:///C:/Windows/win.ini").is_err());
        assert!(Download::start("-o C:/evil").is_err());
    }
}
