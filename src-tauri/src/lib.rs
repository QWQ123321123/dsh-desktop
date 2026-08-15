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

/// Claim the instance lock before any side effect (spawning dsh). A second
/// launch signals the running instance to surface its window and exits.
fn claim_single_instance() -> TcpListener {
    match TcpListener::bind((HOST, LOCK_PORT)) {
        Ok(listener) => listener,
        Err(_) => {
            if let Ok(mut peer) = TcpStream::connect((HOST, LOCK_PORT)) {
                let _ = peer.write_all(b"show");
            }
            println!("[shell] another instance is running; handed off focus, exiting");
            std::process::exit(0);
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
            Self {
                node: "node".into(),
                bin: format!(
                    "{}/../node_modules/@deepseek-ai/dsh/lib/bin.js",
                    env!("CARGO_MANIFEST_DIR")
                )
                .into(),
                dsh_home: None,
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

fn pick_port() -> u16 {
    for port in PORT_FIRST..=PORT_LAST {
        if TcpListener::bind((HOST, port)).is_ok() {
            return port;
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
    cmd.spawn().unwrap_or_else(|e| {
        panic!("failed to spawn `dsh web` via {}: {e}", spec.node.display())
    })
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
        // /T kills the process tree (dsh spawns its own worker children).
        let mut cmd = Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string(), "/T", "/F"]);
        #[cfg(not(debug_assertions))]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let _ = cmd.spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).spawn();
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

/// Current background state as JSON for the in-page panel.
fn bg_state_json() -> String {
    use base64::Engine;
    let image = std::fs::read(bg_image_path()).ok().map(|bytes| {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    });
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
fn serve_control(app: AppHandle) {
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
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                let mut buf = [0u8; 4096];
                let Ok(n) = stream.read(&mut buf) else { return };
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .split('?')
                    .next()
                    .unwrap_or("/");
                let query = req.split_whitespace().nth(1).unwrap_or("");
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
                            .split("v=")
                            .nth(1)
                            .and_then(|s| s.split('&').next()?.parse::<f32>().ok())
                        {
                            set_bg_opacity(v);
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

/// In-page background panel, injected at document start on the dsh UI origin.
/// Renders a shadow-DOM floating button + panel; talks to CONTROL_PORT.
const PANEL_SCRIPT: &str = r#"(() => {
  if (window.top !== window.self) return;
  if (!/^http:\/\/127\.0\.0\.1:\d+$/.test(location.origin)) return;
  const API = 'http://127.0.0.1:3175';
  let panel, slider, overlay;
  async function applyState() {
    try {
      const s = await (await fetch(API + '/bg/state')).json();
      document.getElementById('dsh-shell-bg')?.remove();
      overlay = null;
      if (s.image) {
        overlay = document.createElement('div');
        overlay.id = 'dsh-shell-bg';
        overlay.style.cssText = 'position:fixed;inset:0;z-index:2147483647;pointer-events:none;opacity:' + s.opacity + ';background:url(' + s.image + ') center/cover no-repeat;';
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
        .fab { width:36px;height:36px;border-radius:50%;border:1px solid #dde1e7;background:#fff;
               cursor:pointer;font-size:16px;line-height:1;box-shadow:0 2px 8px rgba(0,0,0,.12); }
        .pop { display:none;position:absolute;bottom:44px;right:0;background:#fff;border:1px solid #dde1e7;
               border-radius:10px;padding:12px;width:190px;box-shadow:0 4px 16px rgba(0,0,0,.15);
               font:12px/1.6 "Segoe UI","Microsoft YaHei",sans-serif;color:#1f2328; }
        .pop.open { display:block; }
        button.act { width:100%;margin:2px 0;padding:5px 8px;border:1px solid #dde1e7;border-radius:6px;
                     background:#f7f8fa;cursor:pointer;font-size:12px; }
        button.act:hover { background:#eef1f6; }
        input[type=range] { width:100%;margin-top:6px; }
        .row { margin-top:8px;color:#8a919c; }
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
    shadow.getElementById('pick').onclick = async () => { await fetch(API + '/bg/pick'); applyState(); };
    shadow.getElementById('clear').onclick = async () => { await fetch(API + '/bg/clear'); applyState(); };
    let t;
    slider.oninput = () => {
      val.textContent = slider.value;
      if (overlay) overlay.style.opacity = slider.value / 100;
      clearTimeout(t);
      t = setTimeout(() => fetch(API + '/bg/opacity?v=' + slider.value / 100), 300);
    };
    slider.oninput();
  }
  function init() { buildPanel(); applyState(); }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();"#;

pub fn run() {
    // Instance lock first: everything after this spawns real processes.
    let lock = claim_single_instance();
    // Shared with the boot thread so ExitRequested can kill the dsh tree.
    let server_pid = std::sync::Arc::new(Mutex::new(None::<u32>));
    let pid_for_boot = std::sync::Arc::clone(&server_pid);

    tauri::Builder::default()
        .setup(move |app| {
            // Focus handoff: a second launch connects to LOCK_PORT; any
            // incoming byte surfaces this instance's window.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                for conn in lock.incoming() {
                    if conn.is_ok() {
                        show_window(&handle);
                    }
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
                        pick_background_file();
                        reload_page_background(app);
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
            serve_control(app.handle().clone());

            // Splash first: the window shows the loading page immediately while
            // dsh boots on a worker thread; ready → navigate to the loopback UI.
            let mut win_builder =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::App("splash.html".into()))
                    .title("DeepSeek Harness")
                    .inner_size(1280.0, 840.0);
            // In-page background panel on the dsh UI origin; state comes from
            // the control channel. initialization_script covers fresh loads;
            // on_page_load re-injects after navigate() (init scripts may only
            // run on the first document in some wry versions).
            win_builder = win_builder.initialization_script(PANEL_SCRIPT).on_page_load(
                |win, payload| {
                    if payload.event() == tauri::webview::PageLoadEvent::Finished {
                        let _ = win.eval(PANEL_SCRIPT);
                    }
                },
            );
            let win = win_builder.build()?;
            #[cfg(debug_assertions)]
            win.open_devtools();

            let win_thread = win.clone();
            std::thread::spawn(move || {
                let spec = ServerSpec::resolve();
                let port = pick_port();
                println!("[shell] picked port {port}");
                let mut child = start_server(&spec, port);
                *pid_for_boot.lock().expect("pid slot") = Some(child.id());
                // wait_ready panics on failure; surface that on the splash page
                // instead of killing the whole shell.
                let ready = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    wait_ready(port, &mut child);
                }));
                match ready {
                    Ok(()) => {
                        println!("[shell] dsh web ready on http://{HOST}:{port}");
                        let url = format!("http://{HOST}:{port}").parse().expect("loopback URL");
                        win_thread.navigate(url).expect("navigate to dsh web UI");
                    }
                    Err(_) => {
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
