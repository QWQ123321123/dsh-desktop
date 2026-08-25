//! Plugin marketplace: relay to upstream `dsh plugin`, which is a thin pnpm
//! forwarder running in `$DSH_HOME/profiles/<name>`. This module owns:
//!
//! - a whitelist gate on package names (upstream forwards pnpm args to
//!   cmd.exe via `shell: true` on Windows, so anything but a registry name
//!   from the UI is a command-injection surface),
//! - a single background job slot for install/remove/ensure-pnpm (pnpm can
//!   take minutes; the control-channel handler must not block on it),
//! - the curated catalog fetch (a JSON file in the GitHub repo, base64 via
//!   the contents API, cached for a few minutes).
//!
//! Listing reads `dsh plugin --profile web list --json` (pnpm's own JSON
//! output) and falls back to the profile's package.json when pnpm is
//! missing or the CLI times out — the UI needs the installed list even when
//! pnpm is absent, so it can show the fix.

use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::engine::Engine;
use serde::{Deserialize, Serialize};

/// Registry names only. Anything else (paths, flags, file:/link: specs,
/// shell metacharacters) must not reach the forwarder.
fn valid_pkg(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('-')
        && !name.starts_with('.')
        && !name.split(['/', '\\']).any(|seg| seg == "..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '/' | '.' | '_' | '-'))
}

/// Minimal percent-decoding for query params (the panel sends
/// encodeURIComponent'd package names; the token check never needed this).
fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Background job (one at a time)

struct PluginJob {
    kind: &'static str, // "install" | "remove" | "pnpm"
    pkg: String,
    state: &'static str, // "running" | "done" | "error"
    message: String,
    output: Arc<Mutex<String>>,
}

static JOB: Mutex<Option<PluginJob>> = Mutex::new(None);

fn set_job_state(state: &'static str, message: String) {
    if let Some(job) = JOB.lock().expect("plugin job").as_mut() {
        job.state = state;
        job.message = message;
    }
}

fn append_output(tail: &Arc<Mutex<String>>, text: &str) {
    let mut t = tail.lock().expect("plugin output");
    t.push_str(text);
    // Keep only the tail: pnpm diagnostics can be long.
    if t.len() > 3000 {
        *t = t[t.len() - 3000..].to_string();
    }
}

/// Run one dsh CLI invocation (`dsh plugin --profile web <args...>`) with the
/// shell's resolved runtime and DSH_HOME, without a console window in release.
fn dsh_plugin_cmd(args: &[&str]) -> Command {
    let (node, bin, home) = crate::dsh_cli();
    let mut cmd = Command::new(&node);
    cmd.arg(&bin).arg("plugin").arg("--profile").arg("web");
    cmd.args(args);
    cmd.env_remove("ELECTRON_RUN_AS_NODE");
    if let Some(h) = &home {
        cmd.env("DSH_HOME", h);
    }
    #[cfg(all(windows, not(debug_assertions)))]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn run_job(kind: &'static str, pkg: String, output: Arc<Mutex<String>>) {
    let sub = match kind {
        "install" => "add",
        "remove" => "remove",
        _ => return,
    };
    let out = dsh_plugin_cmd(&[sub, &pkg])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    match out {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr).into_owned();
            append_output(&output, &text);
            append_output(&output, &err);
            if o.status.success() {
                set_job_state("done", format!("{} 成功", if kind == "install" { "安装" } else { "卸载" }));
            } else if o.status.code() == Some(127) {
                set_job_state(
                    "error",
                    "未找到 pnpm（dsh 插件管理依赖它）。点下方按钮自动安装，或手动执行 npm i -g pnpm".into(),
                );
            } else {
                set_job_state(
                    "error",
                    format!(
                        "pnpm 失败（退出码 {:?}）。常见原因：包名不存在、网络不可达、或 pnpm 10 对 git 依赖的 build 脚本拦截（见输出）",
                        o.status.code()
                    ),
                );
            }
        }
        Err(e) => set_job_state("error", format!("无法启动 dsh CLI：{e}")),
    }
}

fn run_pnpm_job(output: Arc<Mutex<String>>) {
    // npm.cmd needs cmd.exe; Rust's std handles .cmd shims since 1.77.
    let out = Command::new("npm.cmd")
        .args(["install", "-g", "pnpm"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    match out {
        Ok(o) => {
            append_output(&output, &String::from_utf8_lossy(&o.stdout));
            append_output(&output, &String::from_utf8_lossy(&o.stderr));
            if o.status.success() {
                set_job_state("done", "pnpm 安装完成，回到插件市场即可使用".into());
            } else {
                set_job_state(
                    "error",
                    format!("pnpm 安装失败（退出码 {:?}），请手动执行 npm i -g pnpm", o.status.code()),
                );
            }
        }
        Err(e) => set_job_state("error", format!("无法启动 npm：{e}")),
    }
}

/// Spawn a command, wait up to `secs`, kill on timeout. `None` = timed out
/// or failed to spawn (the UI gets the fallback path either way).
fn run_bounded(cmd: &mut Command, secs: u64) -> Option<Output> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Some(child.wait_with_output().ok()?),
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// Installed list

#[derive(Deserialize)]
struct PnpmListProject {
    #[serde(default)]
    dependencies: std::collections::HashMap<String, serde_json::Value>,
    #[serde(default, rename = "devDependencies")]
    dev_dependencies: std::collections::HashMap<String, serde_json::Value>,
}

fn parse_list_json(text: &str) -> Vec<(String, String)> {
    let Ok(v) = serde_json::from_str::<Vec<PnpmListProject>>(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for proj in v {
        for (name, info) in proj.dependencies.into_iter().chain(proj.dev_dependencies) {
            let version = info
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            out.push((name, version));
        }
    }
    out.sort();
    out
}

/// Fallback when pnpm is missing or the CLI timed out: read the profile's
/// package.json directly (the dsh CLI reconciles this file on every run).
fn read_profile_manifest() -> Vec<(String, String)> {
    let (_, _, home) = crate::dsh_cli();
    let Some(home) = home else { return Vec::new() };
    let path = home.join("profiles").join("web").join("package.json");
    let Ok(file) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_reader::<_, serde_json::Value>(file) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key in ["dependencies", "devDependencies"] {
        if let Some(deps) = v.get(key).and_then(|d| d.as_object()) {
            for (name, spec) in deps {
                out.push((name.clone(), spec.as_str().unwrap_or("").to_string()));
            }
        }
    }
    out.sort();
    out
}

pub fn handle_list() -> String {
    let mut plugins: Vec<(String, String)> = Vec::new();
    let mut pnpm = false;
    match run_bounded(&mut dsh_plugin_cmd(&["list", "--json"]), 15) {
        Some(out) if out.status.success() => {
            pnpm = true;
            plugins = parse_list_json(&String::from_utf8_lossy(&out.stdout));
            if plugins.is_empty() {
                // A parse miss still beats an empty list with pnpm present.
                plugins = read_profile_manifest();
            }
        }
        Some(out) if out.status.code() == Some(127) => {
            // dsh's own "pnpm not found" path — report it, still list files.
            pnpm = false;
            plugins = read_profile_manifest();
        }
        _ => {
            pnpm = false;
            plugins = read_profile_manifest();
        }
    }
    let plugins: Vec<serde_json::Value> = plugins
        .into_iter()
        .map(|(name, version)| serde_json::json!({"name": name, "version": version}))
        .collect();
    serde_json::json!({"ok": true, "pnpm": pnpm, "plugins": plugins}).to_string()
}

// ---------------------------------------------------------------------------
// Catalog

#[derive(Clone, Deserialize, Serialize, Default)]
pub struct CatalogEntry {
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub dsh: String,
}

fn catalog_url() -> String {
    std::env::var("DSH_CATALOG_URL").unwrap_or_else(|_| {
        "https://api.github.com/repos/QWQ123321123/dsh-desktop/contents/catalog.json".into()
    })
}

/// Accepts either the contents-API wrapper ({content: base64}) or a plain
/// {plugins: [...]} document, so DSH_CATALOG_URL can point at any static host.
fn parse_catalog(text: &str) -> Result<Vec<CatalogEntry>, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("目录 JSON 解析失败：{e}"))?;
    if let Some(list) = v.get("plugins").and_then(|p| p.as_array()) {
        return serde_json::from_value(serde_json::Value::Array(list.clone()))
            .map_err(|e| format!("目录条目解析失败：{e}"));
    }
    let content = v
        .get("content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| "目录格式无法识别（既不是 {plugins:[...]} 也不是 contents API 包装）".to_string())?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(content.trim())
        .map_err(|e| format!("目录 base64 解码失败：{e}"))?;
    let text = String::from_utf8(decoded).map_err(|e| format!("目录编码异常：{e}"))?;
    parse_catalog(&text)
}

static CATALOG_CACHE: Mutex<Option<(Instant, Vec<CatalogEntry>)>> = Mutex::new(None);

pub fn handle_catalog() -> String {
    {
        let cache = CATALOG_CACHE.lock().expect("catalog cache");
        if let Some((at, entries)) = cache.as_ref() {
            if at.elapsed() < Duration::from_secs(600) {
                return serde_json::json!({"ok": true, "plugins": entries}).to_string();
            }
        }
    }
    let url = catalog_url();
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(30))
        .set("User-Agent", "dsh-desktop-shell/0.3")
        .call();
    let body = match resp {
        Ok(r) => r.into_string().unwrap_or_default(),
        Err(e) => {
            return serde_json::json!({
                "ok": false,
                "error": format!("无法获取插件目录（{e}）。请检查网络后重试。"),
            })
            .to_string()
        }
    };
    match parse_catalog(&body) {
        Ok(entries) => {
            *CATALOG_CACHE.lock().expect("catalog cache") = Some((Instant::now(), entries.clone()));
            serde_json::json!({"ok": true, "plugins": entries}).to_string()
        }
        Err(e) => serde_json::json!({"ok": false, "error": e}).to_string(),
    }
}

// ---------------------------------------------------------------------------
// Handlers for the control channel

fn parse_pkg(query: &str) -> Option<String> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("pkg=").map(pct_decode))
}

pub fn handle_start(kind: &'static str, query: &str) -> String {
    let pkg = if kind == "pnpm" {
        String::new()
    } else {
        match parse_pkg(query) {
            Some(p) => p,
            None => return "{\"ok\":false,\"error\":\"缺少 pkg 参数\"}".into(),
        }
    };
    if kind != "pnpm" && !valid_pkg(&pkg) {
        return serde_json::json!({
            "ok": false,
            "error": "包名不合法：仅接受 npm 注册表包名（如 pkg-name 或 @scope/pkg），不能是路径、参数或特殊字符",
        })
        .to_string();
    }
    let mut guard = JOB.lock().expect("plugin job");
    if let Some(job) = guard.as_ref() {
        if job.state == "running" {
            return serde_json::json!({
                "ok": false,
                "error": format!("已有任务进行中：{} {}", job.kind, job.pkg),
            })
            .to_string();
        }
    }
    let output = Arc::new(Mutex::new(String::new()));
    *guard = Some(PluginJob { kind, pkg: pkg.clone(), state: "running", message: String::new(), output: Arc::clone(&output) });
    drop(guard);
    if kind == "pnpm" {
        std::thread::spawn(move || run_pnpm_job(output));
    } else {
        std::thread::spawn(move || run_job(kind, pkg, output));
    }
    "{\"ok\":true}".into()
}

pub fn handle_status() -> String {
    let job = JOB.lock().expect("plugin job");
    let Some(job) = job.as_ref() else {
        return "{\"ok\":true,\"job\":null}".into();
    };
    let output = job.output.lock().expect("plugin output").clone();
    serde_json::json!({
        "ok": true,
        "job": {
            "state": job.state,
            "kind": job.kind,
            "pkg": job.pkg,
            "message": job.message,
            "output": output,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkg_whitelist() {
        for ok in ["lodash", "@scope/pkg", "pkg-1.2.3_name", "a-b.c_d", "abc123"] {
            assert!(valid_pkg(ok), "{ok} should be valid");
        }
        for bad in [
            "", "-flag", "--help", ".", "..", "../x", "./x", "a/../b", "file:x", "link:x",
            "x y", "x;y", "x&y", "x|y", "x\\y", "http://x", "x$(y)",
        ] {
            assert!(!valid_pkg(bad), "{bad} should be rejected");
        }
    }

    #[test]
    fn pct_decode_roundtrip() {
        assert_eq!(pct_decode("lodash"), "lodash");
        assert_eq!(pct_decode("%40scope%2Fpkg"), "@scope/pkg");
        assert_eq!(pct_decode("a%20b%2Bc"), "a b+c");
        assert_eq!(pct_decode("%zz"), "%zz"); // bad hex passes through
    }

    #[test]
    fn list_json_parses_top_level_deps() {
        let sample = r#"[{"name":"web","version":"0.0.0","private":true,"dependencies":{
            "@deepseek-ai/dsh-base":{"version":"0.1.1","from":"@deepseek-ai/dsh-base","resolved":"https://registry.npmjs.org/..."},
            "lodash":{"version":"4.17.21","from":"lodash"}
        }}]"#;
        let list = parse_list_json(sample);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], ("@deepseek-ai/dsh-base".into(), "0.1.1".into()));
    }

    #[test]
    fn list_json_tolerates_garbage() {
        assert!(parse_list_json("not json").is_empty());
        assert!(parse_list_json("{}").is_empty());
        assert!(parse_list_json("[]").is_empty());
    }

    #[test]
    fn catalog_accepts_plain_and_wrapped() {
        let plain = r#"{"plugins":[{"name":"x","title":"X","author":"a"}]}"#;
        let wrapped = {
            let json = r#"{"plugins":[{"name":"y","version":"1.0.0"}]}"#;
            use base64::engine::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(json);
            format!(r#"{{"content":"{b64}"}}"#)
        };
        let a = parse_catalog(plain).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].name, "x");
        assert_eq!(a[0].author, "a");
        let b = parse_catalog(&wrapped).unwrap();
        assert_eq!(b[0].name, "y");
        assert_eq!(b[0].version, "1.0.0");
        assert!(parse_catalog("garbage").is_err());
    }
}
