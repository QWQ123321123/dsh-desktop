//! Self-contained update checker/installer for the shell.
//!
//! No tauri-plugin-updater: the release pipeline uploads every build to the
//! GitHub Releases of this repo, so the GitHub API is the only server we
//! need. Flow:
//!   1. `check` — newest release tag vs the running version (plain semver).
//!   2. `start` — download the NSIS installer on a worker thread, verifying
//!      SHA-256 against the API's per-asset `digest` (the integrity boundary:
//!      the app has no Authenticode cert, so Windows itself cannot vouch for
//!      the installer and SmartScreen may still warn on first run).
//!   3. `install` — spawn the installer with `/S` and exit the shell; the
//!      exit releases the running exe so NSIS can overwrite it (the
//!      preinstall hook kills the dsh sidecar).
//!
//! `DSH_UPDATE_API_BASE` overrides the API base URL (mirror or tests).

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use sha2::{Digest, Sha256};

pub const REPO: &str = "QWQ123321123/dsh-desktop";
const DEFAULT_API_BASE: &str = "https://api.github.com";
const USER_AGENT: &str = "dsh-desktop-shell-updater";

#[derive(Debug, serde::Deserialize)]
struct GhRelease {
    tag_name: String,
    body: Option<String>,
    published_at: Option<String>,
    assets: Vec<GhAsset>,
}

#[derive(Debug, serde::Deserialize)]
struct GhAsset {
    name: String,
    size: u64,
    digest: Option<String>,
    browser_download_url: String,
}

/// A downloadable update from the newest release.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub tag: String,
    pub notes: String,
    pub size: u64,
    pub url: String,
    pub digest: Option<String>,
}

impl UpdateInfo {
    /// Tag without the leading `v` ("v0.2.3" → "0.2.3").
    pub fn version(&self) -> &str {
        self.tag.trim_start_matches(['v', 'V'])
    }
}

/// Parse "v0.2.3"/"0.2.3" into comparable numbers. Prerelease suffixes are
/// ignored — the shell only ships plain x.y.z tags.
fn parse_ver(s: &str) -> Option<(u64, u64, u64)> {
    let mut it = s.trim().trim_start_matches(['v', 'V']).split('.');
    Some((
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    ))
}

/// `a` is a newer release than `b`. Unparseable input fails closed (never
/// claims an update exists).
pub fn newer(a: &str, b: &str) -> bool {
    match (parse_ver(a), parse_ver(b)) {
        (Some(x), Some(y)) => x > y,
        _ => false,
    }
}

fn api_base() -> String {
    std::env::var("DSH_UPDATE_API_BASE").unwrap_or_else(|_| DEFAULT_API_BASE.into())
}

/// Fetch the newest release that carries a setup installer. Returns `None`
/// when the source has no release; errors are network/parse failures and are
/// surfaced as such.
pub fn check_update(api_base: &str) -> Result<Option<UpdateInfo>, String> {
    let url = format!("{api_base}/repos/{REPO}/releases");
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(30))
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("网络错误：{e}"))?;
    let body = resp
        .into_string()
        .map_err(|e| format!("读取响应失败：{e}"))?;
    let mut releases: Vec<GhRelease> =
        serde_json::from_str(&body).map_err(|e| format!("更新源响应解析失败：{e}"))?;
    // The API returns newest-first, but sort anyway so a mirror that
    // reorders cannot make us pick a stale tag.
    releases.sort_by(|a, b| b.published_at.cmp(&a.published_at));
    Ok(releases.into_iter().find_map(|r| {
        // The setup installer is the only asset the shell ever downloads.
        let asset = r.assets.into_iter().find(|a| a.name.contains("setup"))?;
        Some(UpdateInfo {
            tag: r.tag_name,
            notes: r.body.unwrap_or_default(),
            size: asset.size,
            url: asset.browser_download_url,
            digest: asset.digest,
        })
    }))
}

// ---------------------------------------------------------------- download

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Downloading,
    Verifying,
    Ready,
    Error,
}

#[derive(Debug)]
pub struct DownloadJob {
    pub state: State,
    pub downloaded: u64,
    pub total: u64,
    pub error: Option<String>,
    pub file: Option<PathBuf>,
}

static JOB: Mutex<Option<DownloadJob>> = Mutex::new(None);
static LAST: Mutex<Option<UpdateInfo>> = Mutex::new(None);

/// Updates land in the app data dir, never the install dir (read-only for
/// the packaged app).
fn update_dir() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").expect("%APPDATA%"))
        .join("dsh-desktop-shell-tauri")
        .join("updates")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Stream the installer to `part`, verify its digest, then move it to
/// `final_path`. Progress/state go through `job` so tests can inject their
/// own slot instead of the process-global one.
fn fetch_and_verify(
    info: &UpdateInfo,
    part: &Path,
    final_path: &Path,
    job: &Mutex<Option<DownloadJob>>,
) -> Result<(), String> {
    let resp = ureq::get(&info.url)
        .timeout(Duration::from_secs(600))
        .set("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("下载失败：{e}"))?;
    let total = resp
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(info.size);
    {
        let mut j = job.lock().expect("update job");
        if let Some(j) = &mut *j {
            j.state = State::Downloading;
            j.total = total;
        }
    }
    let mut reader = resp.into_reader();
    let mut file = File::create(part).map_err(|e| format!("创建文件失败：{e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut downloaded = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("读取下载流失败：{e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("写入文件失败：{e}"))?;
        hasher.update(&buf[..n]);
        downloaded += n as u64;
        {
            let mut j = job.lock().expect("update job");
            if let Some(j) = &mut *j {
                j.downloaded = downloaded;
            }
        }
    }
    {
        let mut j = job.lock().expect("update job");
        if let Some(j) = &mut *j {
            j.state = State::Verifying;
        }
    }
    if let Some(digest) = &info.digest {
        let expected = digest
            .strip_prefix("sha256:")
            .unwrap_or(digest)
            .to_ascii_lowercase();
        let actual = hex(&hasher.finalize());
        if expected != actual {
            let _ = std::fs::remove_file(part);
            return Err("安装包校验失败（哈希不匹配），已删除下载文件".into());
        }
    }
    std::fs::rename(part, final_path).map_err(|e| format!("移动文件失败：{e}"))?;
    {
        let mut j = job.lock().expect("update job");
        if let Some(j) = &mut *j {
            j.state = State::Ready;
        }
    }
    Ok(())
}

/// Begin the download on a worker thread. Rejects a second run while one is
/// in flight; a `Ready`/`Error` result may be replaced by a fresh download.
pub fn start_download(info: &UpdateInfo) -> Result<(), String> {
    {
        let j = JOB.lock().expect("update job");
        if let Some(j) = &*j {
            if j.state == State::Downloading || j.state == State::Verifying {
                return Err("已有下载任务进行中".into());
            }
        }
    }
    let dir = update_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建更新目录失败：{e}"))?;
    // A previous round may have left installers/parts behind; clear the slot
    // so the updates dir never accumulates files across versions.
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
    let final_path = dir.join(format!("dsh-desktop_{}_x64-setup.exe", info.version()));
    let part = final_path.with_extension("part");
    *JOB.lock().expect("update job") = Some(DownloadJob {
        state: State::Downloading,
        downloaded: 0,
        total: info.size,
        error: None,
        file: None,
    });
    let info = info.clone();
    std::thread::spawn(move || {
        let result = fetch_and_verify(&info, &part, &final_path, &JOB);
        let mut j = JOB.lock().expect("update job");
        match result {
            // fetch_and_verify already set Ready; record the file.
            Ok(()) => {
                if let Some(j) = &mut *j {
                    j.file = Some(final_path);
                }
            }
            Err(e) => {
                if let Some(j) = &mut *j {
                    j.state = State::Error;
                    j.error = Some(e);
                }
            }
        }
    });
    Ok(())
}

// ---------------------------------------------------------------- endpoints

pub fn handle_check(app: &tauri::AppHandle) -> String {
    let current = app.package_info().version.to_string();
    match check_update(&api_base()) {
        Ok(Some(info)) => {
            let has = newer(info.version(), &current);
            *LAST.lock().expect("last update info") = Some(info.clone());
            serde_json::json!({
                "ok": true,
                "has_update": has,
                "current": current,
                "version": info.version(),
                "tag": info.tag,
                "notes": info.notes,
                "size": info.size,
                "url": info.url,
            })
            .to_string()
        }
        Ok(None) => serde_json::json!({"ok": true, "has_update": false, "current": current}).to_string(),
        Err(e) => serde_json::json!({"ok": false, "error": e}).to_string(),
    }
}

pub fn handle_start() -> String {
    let info = LAST.lock().expect("last update info").clone();
    match info {
        Some(info) => match start_download(&info) {
            Ok(()) => "{\"ok\":true}".into(),
            Err(e) => serde_json::json!({"ok": false, "error": e}).to_string(),
        },
        None => serde_json::json!({"ok": false, "error": "请先检查更新"}).to_string(),
    }
}

pub fn handle_status() -> String {
    let j = JOB.lock().expect("update job");
    match &*j {
        None => "{\"ok\":true,\"state\":\"idle\"}".into(),
        Some(j) => serde_json::json!({
            "ok": true,
            "state": match j.state {
                State::Downloading => "downloading",
                State::Verifying => "verifying",
                State::Ready => "ready",
                State::Error => "error",
            },
            "downloaded": j.downloaded,
            "total": j.total,
            "error": j.error,
            "file": j.file.as_ref().map(|p| p.display().to_string()),
        })
        .to_string(),
    }
}

/// Spawn the verified installer silently and exit the shell. The exit is
/// what releases the running exe for NSIS to overwrite, so the spawn is
/// given a short head start before the process terminates itself.
pub fn handle_install(app: &tauri::AppHandle) -> String {
    #[cfg(windows)]
    let spawn = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let file = JOB
            .lock()
            .expect("update job")
            .as_ref()
            .and_then(|j| j.file.clone())
            .filter(|p| p.is_file())
            .ok_or_else(|| "没有已下载的安装包".to_string());
        file.and_then(|f| {
            std::process::Command::new(&f)
                .arg("/S")
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("启动安装程序失败：{e}"))
        })
    };
    #[cfg(not(windows))]
    let spawn = Err("自动更新仅支持 Windows".to_string());

    match spawn {
        Ok(()) => {
            std::thread::sleep(Duration::from_millis(1500));
            app.exit(0);
            "{\"ok\":true}".into()
        }
        Err(e) => serde_json::json!({"ok": false, "error": e}).to_string(),
    }
}

/// Open the release page of the last checked tag in the default browser
/// (manual-download fallback when the network is hostile to the API).
pub fn handle_open() -> String {
    let tag = LAST.lock().expect("last update info").as_ref().map(|i| i.tag.clone());
    let url = match tag {
        Some(t) => format!("https://github.com/{REPO}/releases/tag/{t}"),
        None => format!("https://github.com/{REPO}/releases"),
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        match std::process::Command::new("cmd")
            .args(["/C", "start", "", &url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            Ok(_) => "{\"ok\":true}".into(),
            Err(e) => serde_json::json!({"ok": false, "error": format!("打开下载页失败：{e}")}).to_string(),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = url;
        "{\"ok\":false,\"error\":\"仅支持 Windows\"}".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One-shot HTTP server answering a canned payload (keeps tests free of
    /// any network dependency).
    fn serve_once(payload: String, content_type: &str) -> (u16, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let ct = content_type.to_string();
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(payload.as_bytes());
        });
        (port, handle)
    }

    #[test]
    fn version_comparison() {
        assert_eq!(parse_ver("v0.2.3"), Some((0, 2, 3)));
        assert_eq!(parse_ver("0.2.10"), Some((0, 2, 10)));
        assert!(parse_ver("0.2").is_none());
        assert!(newer("0.2.10", "0.2.9")); // digit-wise, not lexicographic
        assert!(newer("1.0.0", "0.9.9"));
        assert!(!newer("0.2.3", "0.2.3"));
        assert!(!newer("0.2.3", "0.2.4"));
        assert!(!newer("nonsense", "0.2.3"));
    }

    #[test]
    fn check_picks_newest_setup_asset() {
        let releases = r#"[
          {"tag_name":"v0.2.1","published_at":"2026-07-01T00:00:00Z","body":null,
           "assets":[{"name":"dsh-desktop_0.2.1_x64-setup.exe","size":1,"digest":null,
                      "browser_download_url":"http://127.0.0.1:1/a.exe"}]},
          {"tag_name":"v0.2.3","published_at":"2026-08-01T00:00:00Z","body":"release notes",
           "assets":[{"name":"dsh-desktop_0.2.3_x64-setup.exe","size":2,"digest":null,
                      "browser_download_url":"http://127.0.0.1:1/b.exe"}]}
        ]"#;
        let (port, handle) = serve_once(releases.into(), "application/json");
        let info = check_update(&format!("http://127.0.0.1:{port}"))
            .unwrap()
            .unwrap();
        handle.join().unwrap();
        assert_eq!(info.version(), "0.2.3");
        assert_eq!(info.notes, "release notes");
        assert!(newer(info.version(), "0.2.2"));
        assert!(!newer(info.version(), "0.3.0"));
    }

    #[test]
    fn check_surfaces_bad_source() {
        let (port, handle) = serve_once("not json".into(), "text/plain");
        let err = check_update(&format!("http://127.0.0.1:{port}")).unwrap_err();
        handle.join().unwrap();
        assert!(err.contains("解析失败"), "{err}");
    }

    #[test]
    fn download_verifies_digest_and_tampering() {
        let bytes: Vec<u8> = (0..=255u8).cycle().take(100_000).collect();
        let good = hex(&Sha256::digest(&bytes));

        // (a) matching digest → Ready, file renamed into place
        {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let serve = bytes.clone();
            let h = std::thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    serve.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(&serve);
            });
            let dir = std::env::temp_dir().join(format!("dsh-update-ok-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let part = dir.join("x.part");
            let final_path = dir.join("x.exe");
            let job = Mutex::new(Some(DownloadJob {
                state: State::Downloading,
                downloaded: 0,
                total: bytes.len() as u64,
                error: None,
                file: None,
            }));
            let info = UpdateInfo {
                tag: "v9.9.9".into(),
                notes: String::new(),
                size: bytes.len() as u64,
                url: format!("http://127.0.0.1:{port}/x.exe"),
                digest: Some(format!("sha256:{good}")),
            };
            fetch_and_verify(&info, &part, &final_path, &job).unwrap();
            h.join().unwrap();
            assert_eq!(job.lock().unwrap().as_ref().unwrap().state, State::Ready);
            assert_eq!(
                job.lock().unwrap().as_ref().unwrap().downloaded,
                bytes.len() as u64
            );
            assert_eq!(std::fs::read(&final_path).unwrap(), bytes);
            assert!(!part.exists());
            let _ = std::fs::remove_dir_all(&dir);
        }
        // (b) tampered digest → error, no final file, part cleaned up
        {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let serve = bytes.clone();
            let h = std::thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    serve.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(&serve);
            });
            let dir = std::env::temp_dir().join(format!("dsh-update-bad-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let part = dir.join("x.part");
            let final_path = dir.join("x.exe");
            let job = Mutex::new(Some(DownloadJob {
                state: State::Downloading,
                downloaded: 0,
                total: 0,
                error: None,
                file: None,
            }));
            let info = UpdateInfo {
                tag: "v9.9.9".into(),
                notes: String::new(),
                size: bytes.len() as u64,
                url: format!("http://127.0.0.1:{port}/x.exe"),
                digest: Some("sha256:0000000000000000000000000000000000000000000000000000000000000000".into()),
            };
            let err = fetch_and_verify(&info, &part, &final_path, &job).unwrap_err();
            h.join().unwrap();
            assert!(err.contains("校验失败"), "{err}");
            assert!(!final_path.exists());
            assert!(!part.exists());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
