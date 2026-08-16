//! Tauri 2 shell for `dsh web`: spawns the server under a real Node runtime
//! (Electron-as-Node is fatal to koffi, so the shell itself cannot host it),
//! waits for the loopback port, and loads the Web UI in a WebView2 window.
//!
//! Desktop behaviors, mirroring the Electron prototype:
//! - single instance via a loopback lock port, claimed BEFORE the dsh server
//!   spawns (a plugin-based check would run too late and orphan the server)
//! - port fallback across PORT_RANGE
//! - tray: close hides to tray; real quit via the tray menu

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    webview::WebviewWindowBuilder,
    AppHandle, Manager, WebviewUrl, WindowEvent,
};

const HOST: &str = "127.0.0.1";
const PORT_FIRST: u16 = 3177;
const PORT_LAST: u16 = 3186;
/// Loopback port doubling as the single-instance mutex. Sits just below the
/// server range; the listener dies with the process, so no stale locks.
const LOCK_PORT: u16 = PORT_FIRST - 1;

static QUITTING: AtomicBool = AtomicBool::new(false);

/// Random token gating the 3175 control channel. Only the injected page
/// script knows it; any request without ?t=<token> is rejected. (An Origin
/// allowlist is impossible: in dev the splash page sits on a random port.)
fn make_control_token() -> String {
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf).expect("OS randomness");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Magic reply confirming the peer on LOCK_PORT is another instance of this
/// app (not some unrelated program squatting on the port).
const LOCK_MAGIC: &[u8] = b"dsh-lock-ok";

/// Claim the instance lock before any side effect (spawning dsh). A second
/// launch signals the running instance to surface its window and exits.
fn claim_single_instance() -> TcpListener {
    match TcpListener::bind((HOST, LOCK_PORT)) {
        Ok(listener) => listener,
        Err(_) => {
            let mut is_peer = false;
            if let Ok(mut peer) = TcpStream::connect((HOST, LOCK_PORT)) {
                use std::io::Read;
                let _ = peer.write_all(b"show");
                // A squatter that accepts but never replies must not hang the
                // launcher — bound the handshake.
                let _ = peer.set_read_timeout(Some(Duration::from_millis(800)));
                let mut buf = [0u8; 16];
                is_peer = peer
                    .read(&mut buf)
                    .map(|n| buf[..n] == *LOCK_MAGIC)
                    .unwrap_or(false);
            }
            if is_peer {
                println!("[shell] another instance is running; handed off focus, exiting");
                std::process::exit(0);
            }
            // Lock port held by something else — fail visibly instead of
            // silently exiting ("installed but won't open" with zero signal).
            eprintln!("[shell] lock port {LOCK_PORT} is occupied by another program; refusing to start");
            std::process::exit(1);
        }
    }
}

/// Where the dsh server bits live and how to launch them.
struct ServerSpec {
    /// Node executable: `node` on PATH in dev, the bundled binary when packaged.
    node: std::path::PathBuf,
    /// Entry point of the dsh CLI (`@deepseek-ai/dsh/lib/bin.js`).
    bin: std::path::PathBuf,
    /// Packaged-only: DSH_HOME under the app data dir, and a file for the
    /// child's stdout/stderr (a windows-subsystem release has no console).
    dsh_home: Option<std::path::PathBuf>,
    log_file: Option<std::fs::File>,
}

impl ServerSpec {
    fn resolve() -> Self {
        #[cfg(debug_assertions)]
        {
            // Dev shares the packaged DSH_HOME so credentials/sessions carry
            // over between `tauri dev` and the installed app (splitting them
            // once stranded an older API key in the default ~/.dsh).
            let home = std::path::PathBuf::from(std::env::var("APPDATA").expect("%APPDATA%"))
                .join("dsh-desktop-shell-tauri")
                .join("dsh-home");
            Self {
                node: "node".into(),
                bin: format!(
                    "{}/../node_modules/@deepseek-ai/dsh/lib/bin.js",
                    env!("CARGO_MANIFEST_DIR")
                )
                .into(),
                dsh_home: Some(home),
                log_file: None,
            }
        }
        #[cfg(not(debug_assertions))]
        {
            let exe_dir = std::env::current_exe()
                .expect("current exe path")
                .parent()
                .expect("exe dir")
                .to_path_buf();
            let resources = exe_dir.join("resources");
            let node = resources.join("node/dsh-node.exe");
            let bin = resources.join("dsh-runtime/node_modules/@deepseek-ai/dsh/lib/bin.js");
            assert!(node.is_file(), "bundled node.exe missing at {}", node.display());
            assert!(bin.is_file(), "bundled dsh runtime missing at {}", bin.display());

            let data_dir = std::path::PathBuf::from(std::env::var("APPDATA").expect("%APPDATA%"))
                .join("dsh-desktop-shell-tauri");
            let log_dir = data_dir.join("logs");
            std::fs::create_dir_all(&log_dir).expect("create log dir");
            let log_file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_dir.join("dsh.log"))
                .expect("open dsh log");
            Self {
                node,
                bin,
                dsh_home: Some(data_dir.join("dsh-home")),
                log_file: Some(log_file),
            }
        }
    }
}

/// Returns a listener holding the chosen port plus the port number. Keep the
/// listener alive until the dsh child has been spawned, then drop it so the
/// child can bind — closing the probe-then-spawn race (TOCTOU).
fn pick_port() -> (TcpListener, u16) {
    for port in PORT_FIRST..=PORT_LAST {
        if let Ok(listener) = TcpListener::bind((HOST, port)) {
            return (listener, port);
        }
    }
    panic!("no free port in {PORT_FIRST}..={PORT_LAST}");
}

/// Absolute path to the dsh CLI entry inside the workspace's node_modules.
/// (Dev-only helper kept for readability; release resolves via ServerSpec.)
fn start_server(spec: &ServerSpec, port: u16) -> Child {
    let mut cmd = Command::new(&spec.node);
    cmd.args([
        spec.bin.to_string_lossy().into_owned(),
        "web".to_string(),
        "--port".to_string(),
        port.to_string(),
    ])
    // An Electron-based IDE terminal exports this; it must not leak into
    // the Node child (every spawned binary would degrade to plain Node).
    .env_remove("ELECTRON_RUN_AS_NODE");
    if let Some(home) = &spec.dsh_home {
        std::fs::create_dir_all(home).expect("create DSH_HOME");
        cmd.env("DSH_HOME", home);
    }
    if let Some(log) = &spec.log_file {
        cmd.stdout(log.try_clone().expect("clone log handle"))
            .stderr(log.try_clone().expect("clone log handle"));
    }
    // Packaged GUI app: node.exe is a console-subsystem binary and would
    // otherwise pop a console window. Dev keeps the child's console visible.
    #[cfg(all(windows, not(debug_assertions)))]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let child = cmd.spawn().unwrap_or_else(|e| {
        panic!("failed to spawn `dsh web` via {}: {e}", spec.node.display())
    });
    tie_child_lifetime(&child);
    child
}

/// Tie the dsh child's lifetime to this process via a Windows Job Object:
/// however the shell dies (crash, taskkill, an agent killing its own host),
/// the OS kills the whole child tree. Without this, a dead shell orphans
/// `dsh-node.exe` holding the port. Best-effort: failure only loses the
/// guarantee, never breaks the spawn.
#[cfg(windows)]
fn tie_child_lifetime(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
        JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            eprintln!("[shell] CreateJobObjectW failed; child lifetime not tied");
            return;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            eprintln!("[shell] SetInformationJobObject failed; child lifetime not tied");
            CloseHandle(job);
            return;
        }
        if AssignProcessToJobObject(job, child.as_raw_handle() as _) == 0 {
            eprintln!("[shell] AssignProcessToJobObject failed; child lifetime not tied");
            CloseHandle(job);
            return;
        }
        // Deliberately never closed: the job must outlive the child; process
        // exit releases it (and the limit flag kills the child tree with us).
    }
}

/// True once the server answers `GET /` with a 2xx/3xx — a bare TCP connect
/// fires too early (listener up before the webserver plugin serves), which
/// raced the webview into a half-booted plugin tree.
fn http_ready(port: u16) -> bool {
    use std::io::{Read, Write};
    let addr = format!("{HOST}:{port}").parse().unwrap();
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
        return false;
    };
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok();
    if stream
        .write_all(format!("GET / HTTP/1.0\r\nHost: {HOST}:{port}\r\n\r\n").as_bytes())
        .is_err()
    {
        return false;
    }
    let mut head = [0u8; 15]; // "HTTP/1.1 200 " fits
    matches!(stream.read(&mut head), Ok(n) if n >= 12 && head[9] == b'2' || n >= 12 && head[9] == b'3')
}

fn wait_ready(port: u16, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("failed to poll dsh child") {
            panic!("dsh web exited before becoming ready: {status}");
        }
        if http_ready(port) {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("timed out waiting for dsh web server on port {port}");
}

fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // Graceful attempt first (no /F; on a console-less node this may be a
        // no-op — the forced pass is the backstop). Both passes wait on
        // .status() so the shell's exit never races the kill.
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        std::thread::sleep(Duration::from_millis(1500));
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
    }
}

fn show_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
    }
}

// ------------------------------------------------------- custom background

/// Loopback command channel for the in-page background panel. The webview
/// loads a remote (http) origin, so Tauri IPC is unavailable; this plain-HTTP
/// listener is the bridge. Sits below the server/lock ports.
const CONTROL_PORT: u16 = PORT_FIRST - 2;

/// App data dir; dev and release share it so settings carry over.
fn data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("APPDATA").expect("%APPDATA%"))
        .join("dsh-desktop-shell-tauri")
}

fn bg_image_path() -> std::path::PathBuf {
    data_dir().join("background.img")
}

fn bg_opacity_path() -> std::path::PathBuf {
    data_dir().join("background.opacity")
}

fn bg_opacity() -> f32 {
    std::fs::read_to_string(bg_opacity_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0.18)
}

fn set_bg_opacity(v: f32) {
    let clamped = v.clamp(0.02, 0.6);
    std::fs::create_dir_all(data_dir()).expect("create data dir");
    std::fs::write(bg_opacity_path(), clamped.to_string()).expect("store opacity");
}

/// Cache of (file mtime, base64 data URI) for the background image so
/// repeated /bg/state polls don't re-read and re-encode a large file.
static BG_IMAGE_CACHE: Mutex<Option<(std::time::SystemTime, String)>> = Mutex::new(None);

/// Current background state as JSON for the in-page panel.
fn bg_state_json() -> String {
    use base64::Engine;
    let path = bg_image_path();
    let mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
    let mut cache = BG_IMAGE_CACHE.lock().expect("bg image cache");
    let image = match (&*cache, mtime) {
        (Some((t, s)), Some(m)) if *t == m => Some(s.clone()),
        _ => {
            let encoded = std::fs::read(&path).ok().map(|bytes| {
                format!(
                    "data:image/png;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                )
            });
            if let (Some(m), Some(s)) = (mtime, &encoded) {
                *cache = Some((m, s.clone()));
            }
            encoded
        }
    };
    format!(
        "{{\"opacity\":{},\"image\":{}}}",
        bg_opacity(),
        image.map(|s| format!("\"{s}\"")).unwrap_or("null".into())
    )
}

fn pick_background_file() -> bool {
    let picked = rfd::FileDialog::new()
        .add_filter("图片", &["png", "jpg", "jpeg", "webp", "gif", "bmp"])
        .pick_file();
    if let Some(file) = picked {
        std::fs::create_dir_all(data_dir()).expect("create data dir");
        std::fs::copy(file, bg_image_path()).expect("store background image");
        return true;
    }
    false
}

fn clear_background_file() {
    let _ = std::fs::remove_file(bg_image_path());
}

/// Tell the page to re-pull background state (after tray-menu changes).
fn reload_page_background(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.eval("window.__dshBgReload && window.__dshBgReload()");
    }
}

/// Minimal plain-HTTP responder for the in-page panel. GET-only, loopback,
/// permissive CORS (the dsh page origin differs by port, so preflight-free
/// GETs still need ACAO on the response).
fn serve_control(
    app: AppHandle,
    ready_port: std::sync::Arc<Mutex<Option<u16>>>,
    token: String,
) {
    let listener = match TcpListener::bind((HOST, CONTROL_PORT)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[shell] control port {CONTROL_PORT} unavailable: {e} — in-page background panel disabled");
            return;
        }
    };
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut stream) = conn else { continue };
            let app = app.clone();
            let ready_port = std::sync::Arc::clone(&ready_port);
            let token = token.clone();
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                let mut buf = [0u8; 4096];
                let Ok(n) = stream.read(&mut buf) else { return };
                let req = String::from_utf8_lossy(&buf[..n]);
                let mut parts = req.split_whitespace();
                let method = parts.next().unwrap_or("");
                let target = parts.next().unwrap_or("/");
                let (path, query) = target.split_once('?').unwrap_or((target, ""));

                // Token gate: only the injected page script knows the token.
                let authed = method == "GET"
                    && query
                        .split('&')
                        .any(|kv| kv.strip_prefix("t=").map(|v| v == token).unwrap_or(false));
                if !authed {
                    let _ = stream.write_all(
                        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    return;
                }

                let body = match path {
                    "/bg/state" => bg_state_json(),
                    "/bg/pick" => {
                        let changed = pick_background_file();
                        reload_page_background(&app);
                        format!("{{\"ok\":{changed}}}")
                    }
                    "/bg/clear" => {
                        clear_background_file();
                        reload_page_background(&app);
                        "{\"ok\":true}".into()
                    }
                    "/bg/opacity" => {
                        if let Some(v) = query
                            .split('&')
                            .find_map(|kv| kv.strip_prefix("v="))
                            .and_then(|s| s.parse::<f32>().ok())
                        {
                            set_bg_opacity(v);
                        }
                        "{\"ok\":true}".into()
                    }
                    "/win/state" => {
                        match *ready_port.lock().expect("ready port") {
                            Some(p) => format!("{{\"url\":\"http://{HOST}:{p}\"}}"),
                            None => "{\"url\":null}".into(),
                        }
                    }
                    // Window chrome controls for the in-page titlebar
                    // (frameless window: drag/min/max/close ride this channel).
                    p if p.starts_with("/win/") => {
                        let win = app.get_webview_window("main");
                        match p {
                            "/win/drag" => {
                                if let Some(w) = &win { let _ = w.start_dragging(); }
                            }
                            "/win/min" => {
                                if let Some(w) = &win { let _ = w.minimize(); }
                            }
                            "/win/max" => {
                                if let Some(w) = &win {
                                    if w.is_maximized().unwrap_or(false) { let _ = w.unmaximize(); }
                                    else { let _ = w.maximize(); }
                                }
                            }
                            "/win/fullscreen" => {
                                // intentionally unsupported on the frameless
                                // window: set_fullscreen swaps in WS_POPUP and
                                // the restore path breaks border resizing.
                            }
                            "/win/devtools" => {
                                #[cfg(debug_assertions)]
                                if let Some(w) = &win { w.open_devtools(); }
                            }
                            // close hides to tray (same semantics as the window X)
                            "/win/close" => {
                                if let Some(w) = &win { let _ = w.hide(); }
                            }
                            "/win/quit" => app.exit(0),
                            "/win/about" => {
                                let ver = app.package_info().version.to_string();
                                rfd::MessageDialog::new()
                                    .set_title("关于 DeepSeek Harness")
                                    .set_description(&format!(
                                        "DeepSeek Harness 桌面客户端\nTauri 2 壳 + dsh web 后端\n版本 {ver}"
                                    ))
                                    .show();
                            }
                            _ => {}
                        }
                        "{\"ok\":true}".into()
                    }
                    _ => "{\"ok\":false}".into(),
                };
                let res = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(res.as_bytes());
            });
        }
    });
}

/// In-page chrome for the frameless window: a custom titlebar (menus, drag,
/// window controls via CONTROL_PORT) plus the background panel and the
/// settings-page CSS override. Titlebar/chrome run on every page (splash
/// included); the background panel only on the dsh UI origin.
const PANEL_SCRIPT: &str = r##"(() => {
  // Runs twice per page (initialization_script + on_page_load fallback);
  // guard everything once-per-page-load.
  if (window.__dshShellBooted) return;
  window.__dshShellBooted = true;
  const API = 'http://127.0.0.1:3175';
  // Origin detection is subtle: in dev, wry serves frontendDist from a random
  // 127.0.0.1 port, so a bare loopback regex misfires on the splash page.
  // Identify splash by path, the dsh UI by its known port range.
  const pagePort = Number(location.port);
  const onDsh = location.hostname === '127.0.0.1' && pagePort >= 3177 && pagePort <= 3186;
  const onSplash = location.pathname.endsWith('splash.html');
  const TOKEN = '__DSH_TOKEN__'; // replaced by the shell at injection time
  function api(p) {
    return fetch(API + p + (p.includes('?') ? '&' : '?') + 't=' + encodeURIComponent(TOKEN));
  }
  function cmd(p) { return api(p).catch(() => {}); }
  function clickDsh(text) {
    [...document.querySelectorAll('button')].find(b => b.textContent.trim() === text)?.click();
  }

  // ---------------- custom titlebar (Codex-style chrome) ----------------
  const MENUS = [
    { label: '文件', items: [
      { label: '新建会话', key: 'Ctrl+N', act: () => clickDsh('新会话') },
      { label: '设置', key: 'Ctrl+,', act: () => clickDsh('设置') },
      '-',
      { label: '退出', act: () => cmd('/win/quit') },
    ]},
    { label: '编辑', items: [
      // 只留全选：剪贴/复制/粘贴在 React 受控输入框上靠 execCommand 必然失效，
      // 原生右键菜单（M3 放行后）已覆盖这些编辑操作。
      { label: '全选', key: 'Ctrl+A', act: () => document.execCommand('selectAll') },
    ]},
    { label: '视图', items: [
      { label: '刷新界面', key: 'Ctrl+R', act: () => location.reload() },
      { label: '开发者工具', key: 'F12', act: () => cmd('/win/devtools') },
      // 不提供全屏：无边框窗口 set_fullscreen 换 WS_POPUP 样式后还原不干净，
      // 会导致边框缩放失效；最大化已覆盖该场景。
    ]},
    { label: '帮助', items: [
      { label: '关于 DeepSeek Harness', act: () => cmd('/win/about') },
    ]},
  ];

  function buildTitlebar() {
    if (document.getElementById('dsh-shell-titlebar')) return;
    const host = document.createElement('div');
    host.id = 'dsh-shell-titlebar';
    const shadow = host.attachShadow({ mode: 'open' });
    shadow.innerHTML = `
      <style>
        .bar { position:fixed; top:0; left:0; right:0; height:32px; z-index:2147483646;
               display:flex; align-items:center; justify-content:space-between;
               font:13px/1 "Segoe UI","Microsoft YaHei",sans-serif; user-select:none;
               color: light-dark(#1f2328, #e8eaed);
               text-shadow: 0 1px 3px light-dark(rgba(255,255,255,.5), rgba(0,0,0,.5));
               /* transparent fill: the page background image must show through
                  (a filled bar over the margin-top strip caused a visible seam);
                  backdrop blur alone keeps labels readable over any image */
               backdrop-filter: blur(12px); -webkit-backdrop-filter: blur(12px); }
        .left { display:flex; align-items:center; height:100%; }
        .title { padding:0 10px; opacity:.6; font-size:12px; }
        .mbtn { padding:0 10px; height:100%; display:flex; align-items:center; cursor:default; border-radius:4px;
                transition: background .15s ease, transform .08s ease; }
        .mbtn:hover { background:rgba(127,127,127,.18); }
        .mbtn:active { transform: scale(.94); }
        .drop { position:absolute; top:32px; min-width:210px; padding:4px; border-radius:8px;
                background: light-dark(rgba(252,252,252,.97), rgba(42,42,45,.97));
                box-shadow:0 6px 20px rgba(0,0,0,.28);
                animation: dshDropIn .14s ease-out; }
        @keyframes dshDropIn { from { opacity: 0; transform: translateY(-4px); } to { opacity: 1; transform: none; } }
        .mi { display:flex; justify-content:space-between; gap:24px; padding:6px 12px; border-radius:5px; cursor:default;
              transition: background .12s ease; }
        .mi:hover { background:rgba(77,107,254,.18); }
        .mi:active { transform: scale(.98); }
        .sep { height:1px; margin:4px 8px; background:rgba(127,127,127,.25); }
        .key { opacity:.5; font-size:11px; }
        .right { display:flex; height:100%; }
        .wbtn { width:44px; display:flex; align-items:center; justify-content:center; font-size:12px;
                transition: background .15s ease, color .15s ease; }
        .wbtn:hover { background:rgba(127,127,127,.18); }
        .wbtn.close:hover { background:#e81123; color:#fff; }
      </style>
      <div class="bar">
        <div class="left"><span class="title">DeepSeek Harness</span></div>
        <div class="right">
          <div class="wbtn" id="wmin">—</div>
          <div class="wbtn" id="wmax">▢</div>
          <div class="wbtn close" id="wclose">✕</div>
        </div>
      </div>`;
    document.documentElement.appendChild(host);
    const bar = shadow.querySelector('.bar');
    const left = shadow.querySelector('.left');
    let openDrop = null;
    const closeDrop = () => { openDrop?.remove(); openDrop = null; };
    // Shadow DOM retargets event.target to the host for document-level
    // listeners, so watch BOTH levels: inside the shadow root (real targets)
    // for presses on the bar outside the dropdown, and at the document for
    // presses anywhere else in the page.
    shadow.addEventListener('mousedown', (e) => {
      if (openDrop && !openDrop.contains(e.target)) closeDrop();
    });
    document.addEventListener('mousedown', (e) => {
      if (openDrop && e.target !== host) closeDrop();
    });
    for (const m of MENUS) {
      const btn = document.createElement('div');
      btn.className = 'mbtn';
      btn.textContent = m.label;
      btn.onmousedown = e => e.stopPropagation();
      btn.onclick = () => {
        if (openDrop) { closeDrop(); return; }
        const drop = document.createElement('div');
        drop.className = 'drop';
        drop.style.left = btn.offsetLeft + 'px';
        for (const it of m.items) {
          if (it === '-') {
            const s = document.createElement('div');
            s.className = 'sep';
            drop.appendChild(s);
            continue;
          }
          const row = document.createElement('div');
          row.className = 'mi';
          const l = document.createElement('span'); l.textContent = it.label;
          const k = document.createElement('span'); k.className = 'key'; k.textContent = it.key ?? '';
          row.append(l, k);
          row.onclick = () => { closeDrop(); it.act(); };
          drop.appendChild(row);
        }
        bar.appendChild(drop);
        openDrop = drop;
      };
      left.appendChild(btn);
    }
    bar.onmousedown = e => {
      if (e.target === bar || e.target === left || e.target.classList?.contains('title')) cmd('/win/drag');
    };
    bar.ondblclick = e => { if (e.target === bar || e.target === left) cmd('/win/max'); };
    shadow.getElementById('wmin').onclick = () => cmd('/win/min');
    shadow.getElementById('wmax').onclick = () => cmd('/win/max');
    shadow.getElementById('wclose').onclick = () => cmd('/win/close');
  }

  // Reserve the titlebar strip; dsh full-height layouts tolerate this because
  // their scroll containers are internal flex children. Also hosts the global
  // page-entry animation (subtle fade/slide on every load) with a
  // reduced-motion escape hatch.
  function installChromeStyle() {
    if (document.getElementById('dsh-shell-chrome')) return;
    const style = document.createElement('style');
    style.id = 'dsh-shell-chrome';
    style.textContent = `
      html { margin-top: 32px !important; height: calc(100% - 32px) !important; overflow: hidden !important; }
      @keyframes dshPageIn { from { opacity: 0; transform: translateY(6px); } to { opacity: 1; transform: none; } }
      @media (prefers-reduced-motion: no-preference) {
        body { animation: dshPageIn .28s ease-out; }
      }`;
    document.documentElement.appendChild(style);
  }

  document.addEventListener('keydown', e => {
    if (e.ctrlKey && !e.shiftKey && (e.key === 'n' || e.key === 'N')) { e.preventDefault(); clickDsh('新会话'); }
    if (e.ctrlKey && e.key === ',') { e.preventDefault(); clickDsh('设置'); }
    if (e.key === 'F12') { e.preventDefault(); cmd('/win/devtools'); }
  });

  // Block the browser chrome context menu (back/forward/reload — "back"
  // strands the window on the splash page); keep it on text fields for
  // editing actions.
  document.addEventListener('contextmenu', e => {
    const t = e.target;
    if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable)) return;
    e.preventDefault();
  });

  // Splash self-heals: poll the control channel for the ready URL instead of
  // relying on the boot thread's one-shot navigate(). Fade out before leaving
  // so the splash → UI switch reads as a transition, not a flash.
  if (onSplash) {
    const poll = setInterval(async () => {
      try {
        const r = await (await api('/win/state')).json();
        if (r.url) {
          clearInterval(poll);
          document.body.style.transition = 'opacity .25s ease';
          document.body.style.opacity = '0';
          setTimeout(() => location.replace(r.url), 260);
        }
      } catch (e) { /* shell not up yet */ }
    }, 800);
  }

  // ---------------- background panel (dsh UI origin only) ----------------
  let panel, slider, overlay;
  async function applyState() {
    try {
      const s = await (await api('/bg/state')).json();
      document.getElementById('dsh-shell-bg')?.remove();
      overlay = null;
      if (s.image) {
        overlay = document.createElement('div');
        overlay.id = 'dsh-shell-bg';
        overlay.style.cssText = 'position:fixed;inset:0;z-index:2147483645;pointer-events:none;opacity:' + s.opacity + ';background:url(' + s.image + ') center/cover no-repeat;';
        document.documentElement.appendChild(overlay);
      }
      if (slider) slider.value = Math.round(s.opacity * 100);
    } catch (e) { /* shell control channel down: stay silent */ }
  }
  window.__dshBgReload = applyState;
  function buildPanel() {
    if (document.getElementById('dsh-shell-bg-panel')) return;
    const host = document.createElement('div');
    host.id = 'dsh-shell-bg-panel';
    host.style.cssText = 'position:fixed;right:16px;bottom:16px;z-index:2147483647;';
    const shadow = host.attachShadow({ mode: 'open' });
    shadow.innerHTML = `
      <style>
        /* light-dark() follows dsh's own theme toggle: dsh sets
           color-scheme on the root element, which inherits into shadow DOM. */
        .fab { width:36px;height:36px;border-radius:50%;border:1px solid light-dark(#dde1e7,#44464c);
               background: light-dark(#fff,#2a2a2d);
               cursor:pointer;font-size:16px;line-height:1;box-shadow:0 2px 8px rgba(0,0,0,.12); }
        .pop { display:none;position:absolute;bottom:44px;right:0;
               background: light-dark(#fff,#2a2a2d);border:1px solid light-dark(#dde1e7,#44464c);
               border-radius:10px;padding:12px;width:190px;box-shadow:0 4px 16px rgba(0,0,0,.15);
               font:12px/1.6 "Segoe UI","Microsoft YaHei",sans-serif;color: light-dark(#1f2328,#e8eaed); }
        .pop.open { display:block; }
        button.act { width:100%;margin:2px 0;padding:5px 8px;border:1px solid light-dark(#dde1e7,#44464c);
                     border-radius:6px;background: light-dark(#f7f8fa,#3a3d44);cursor:pointer;font-size:12px;
                     color: light-dark(#1f2328,#e8eaed); transition: background .12s ease, transform .08s ease; }
        button.act:hover { background: light-dark(#eef1f6,#484b54); }
        button.act:active { transform: scale(.97); }
        .fab { transition: transform .12s ease, box-shadow .12s ease; }
        .fab:hover { transform: scale(1.08); box-shadow:0 4px 12px rgba(0,0,0,.2); }
        .fab:active { transform: scale(.94); }
        .pop { animation: dshPopIn .16s ease-out; }
        @keyframes dshPopIn { from { opacity: 0; transform: translateY(6px) scale(.96); } to { opacity: 1; transform: none; } }
        input[type=range] { width:100%;margin-top:6px; }
        .row { margin-top:8px;color: light-dark(#8a919c,#9aa0a8); }
      </style>
      <button class="fab" title="背景设置">🎨</button>
      <div class="pop">
        <button class="act" id="pick">选择背景图片…</button>
        <button class="act" id="clear">清除背景</button>
        <div class="row">透明度 <span id="val"></span>%</div>
        <input type="range" id="op" min="2" max="60" value="18">
      </div>`;
    document.documentElement.appendChild(host);
    panel = shadow.querySelector('.pop');
    slider = shadow.getElementById('op');
    const val = shadow.getElementById('val');
    shadow.querySelector('.fab').onclick = () => panel.classList.toggle('open');
    shadow.getElementById('pick').onclick = async () => { await api('/bg/pick'); applyState(); };
    shadow.getElementById('clear').onclick = async () => { await api('/bg/clear'); applyState(); };
    let t;
    slider.oninput = () => {
      val.textContent = slider.value;
      if (overlay) overlay.style.opacity = slider.value / 100;
      clearTimeout(t);
      t = setTimeout(() => api('/bg/opacity?v=' + slider.value / 100), 300);
    };
    slider.oninput();
  }
  // dsh 设置弹窗 → 整页：SettingsRoot 的结构是 overlay > mask + panel(nav)，
  // 用结构选择器命中（CSS Modules 的类名带哈希，不能硬编码）。
  // top 留出 32px 标题栏，窗口控件保持可用。
  function installSettingsPageStyle() {
    if (document.getElementById('dsh-shell-settings-page')) return;
    const style = document.createElement('style');
    style.id = 'dsh-shell-settings-page';
    style.textContent = `
      [role="presentation"]:has(> [role="dialog"][aria-modal="true"] nav) { position: fixed; inset: 0; }
      [role="presentation"]:has(> [role="dialog"][aria-modal="true"] nav) > [aria-hidden="true"] { display: none; }
      [role="dialog"][aria-modal="true"]:has(nav) {
        position: fixed !important; inset: 32px 0 0 0 !important;
        width: 100vw !important; height: calc(100vh - 32px) !important;
        max-width: none !important; max-height: none !important;
        margin: 0 !important; border-radius: 0 !important; border: none !important;
      }
      @media (prefers-reduced-motion: no-preference) {
        [role="dialog"][aria-modal="true"]:has(nav) { animation: dshPageIn .22s ease-out; }
      }`;
    document.documentElement.appendChild(style);
  }

  function init() {
    buildTitlebar();
    installChromeStyle();
    if (onDsh) { buildPanel(); applyState(); installSettingsPageStyle(); }
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})()"##;

pub fn run() {
    // Instance lock first: everything after this spawns real processes.
    let lock = claim_single_instance();
    // Shared with the boot thread so ExitRequested can kill the dsh tree.
    let server_pid = std::sync::Arc::new(Mutex::new(None::<u32>));
    let pid_for_boot = std::sync::Arc::clone(&server_pid);
    // Set once dsh is ready; the splash page polls /win/state for it and
    // navigates itself (self-healing if the window ever lands back on splash).
    let ready_port = std::sync::Arc::new(Mutex::new(None::<u16>));
    let port_for_boot = std::sync::Arc::clone(&ready_port);
    let port_for_control = std::sync::Arc::clone(&ready_port);

    tauri::Builder::default()
        .setup(move |app| {
            // Focus handoff: a second launch connects to LOCK_PORT and says
            // "show"; we answer with LOCK_MAGIC so squatters are detectable.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                for conn in lock.incoming() {
                    let Ok(mut stream) = conn else { continue };
                    let handle = handle.clone();
                    std::thread::spawn(move || {
                        use std::io::{Read, Write};
                        let mut buf = [0u8; 16];
                        let _ = stream.read(&mut buf);
                        let _ = stream.write_all(LOCK_MAGIC);
                        show_window(&handle);
                    });
                }
            });
            let show = MenuItemBuilder::with_id("show", "显示 DeepSeek Harness").build(app)?;
            let bg = MenuItemBuilder::with_id("bg", "设置背景…").build(app)?;
            let bg_clear = MenuItemBuilder::with_id("bg_clear", "清除背景").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "退出").build(app)?;
            let menu = MenuBuilder::new(app)
                .items(&[&show, &bg, &bg_clear, &quit])
                .build()?;
            TrayIconBuilder::with_id("main")
                .tooltip("DeepSeek Harness")
                .icon(app.default_window_icon().expect("window icon").clone())
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show_window(app),
                    "bg" => {
                        // rfd's modal dialog blocks the caller; keep it off
                        // the main (event-loop) thread.
                        let app = app.clone();
                        std::thread::spawn(move || {
                            pick_background_file();
                            reload_page_background(&app);
                        });
                    }
                    "bg_clear" => {
                        clear_background_file();
                        reload_page_background(app);
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_window(tray.app_handle());
                    }
                })
                .build(app)?;
            println!("[shell] tray created");
            // Token-gate the control channel; only the injected script knows it.
            let token = make_control_token();
            let panel_script = PANEL_SCRIPT.replace("__DSH_TOKEN__", &token);
            serve_control(app.handle().clone(), port_for_control, token);

            // Splash first: the window shows the loading page immediately while
            // dsh boots on a worker thread; ready → splash navigates itself via /win/state.
            let mut win_builder =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("splash.html".into()))
                    .title("DeepSeek Harness")
                    .inner_size(1280.0, 840.0)
                    // Frameless: the in-page titlebar (PANEL_SCRIPT) provides
                    // menus, drag, and window controls via the control channel.
                    .decorations(false);
            // In-page chrome on every page; initialization_script covers fresh
            // loads, on_page_load re-injects after navigations (init scripts
            // may only run on the first document in some wry versions).
            win_builder = win_builder
                .initialization_script(&panel_script)
                .on_page_load(move |win, payload| {
                    if payload.event() == tauri::webview::PageLoadEvent::Finished {
                        let _ = win.eval(&panel_script);
                    }
                });
            let win = win_builder.build()?;
            // DevTools on demand only: 视图 → 开发者工具 (F12). Auto-opening it
            // also enabled the device-toolbar viewport badge ("1280px × 840px").

            let win_thread = win.clone();
            std::thread::spawn(move || {
                // Everything that can panic lives inside the boundary: a dead
                // boot thread would otherwise leave the splash stuck forever.
                let boot = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let spec = ServerSpec::resolve();
                    // Hold the port until the dsh child exists (TOCTOU guard),
                    // then release it for the child to bind.
                    let (_guard, port) = pick_port();
                    println!("[shell] picked port {port}");
                    let mut child = start_server(&spec, port);
                    drop(_guard);
                    *pid_for_boot.lock().expect("pid slot") = Some(child.id());
                    wait_ready(port, &mut child);
                    port
                }));
                match boot {
                    Ok(port) => {
                        println!("[shell] dsh web ready on http://{HOST}:{port}");
                        // The splash page polls /win/state and navigates itself.
                        *port_for_boot.lock().expect("port slot") = Some(port);
                    }
                    Err(_) => {
                        // Clean up a half-started dsh tree before the error.
                        if let Some(pid) = *pid_for_boot.lock().expect("pid slot") {
                            kill_tree(pid);
                        }
                        let _ = win_thread.eval(
                            "document.getElementById('status').textContent = '后端启动失败，请查看日志后重启应用';",
                        );
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if !QUITTING.load(Ordering::SeqCst) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |_app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                QUITTING.store(true, Ordering::SeqCst);
                if let Some(pid) = *server_pid.lock().expect("pid slot") {
                    kill_tree(pid);
                }
            }
        });
}
