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
const PANEL_SCRIPT: &str = include_str!("../assets/shell-panel.js");

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
