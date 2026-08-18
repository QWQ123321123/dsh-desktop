# deepseek-harness-desktop

[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)（`dsh web`）的 Windows 桌面客户端壳，Tauri 2 实现。

壳负责拉起内嵌的 `dsh web` 后端（loopback HTTP），用系统 WebView2 加载其 Web UI，并提供桌面化能力：启动动画、托盘常驻、单实例、端口退让、页面内背景自定义、NSIS 安装包。

## 功能

- **启动动画**：窗口立即显示 splash（鲸鱼呼吸动画），后端就绪后自动跳转到主界面；启动失败在 splash 页提示而不是崩溃
- **托盘常驻**：关窗最小化到托盘；托盘菜单「显示 / 设置背景… / 清除背景 / 退出」
- **背景自定义**：页面右下角 🎨 悬浮面板（选择图片 / 清除 / 透明度滑块），托盘菜单同步可用；配置存于应用数据目录，卸载保留
- **单实例**：重复启动聚焦已有窗口；实例锁在 spawn 后端**之前**抢占（见「已知坑」）
- **端口退让**：后端端口在 3177–3186 间自动选择
- **零外部运行时依赖**：安装包内嵌 Node 运行时（重命名为 `dsh-node.exe`）与 dsh 生产依赖树

## 环境前提

- Windows 10/11，已装 WebView2 运行时（系统一般自带）
- [rustup](https://rustup.rs/) + stable-x86_64-pc-windows-msvc 工具链（国内建议 `RUSTUP_DIST_SERVER=https://rsproxy.cn`）
- Visual Studio 2019+ Build Tools（C++ 工作负荷，提供 MSVC 链接器）
- Node.js ≥ 22.19（仅开发期用于 npm/脚本；运行时用打包内的 `dsh-node.exe`）
- crates.io 镜像（可选）：项目已带 `.cargo/config.toml`（rsproxy sparse）

## 构建

```sh
npm install                        # 装 @tauri-apps/cli 等

# 1. 准备运行时资源（写入 src-tauri/resources/，此目录不入库）
mkdir -p src-tauri/resources/dsh-runtime
cd src-tauri/resources/dsh-runtime
echo '{"name":"dsh-runtime","private":true,"dependencies":{"@deepseek-ai/dsh":"0.1.0-rc.6"}}' > package.json
npm install --omit=dev --no-audit --no-fund
cd ../../..
node scripts/prune-runtime.cjs     # 裁剪 .map/.d.ts/非 win32-x64 原生二进制等
# 把官方 node.exe 放到 src-tauri/resources/node/dsh-node.exe（必须改名，见「已知坑」）

# 2. 生成图标与 splash
node scripts/gen-icons.cjs         # assets/deepseek-logo.svg → src-tauri/icons/
node scripts/gen-splash.cjs        # → dist/splash.html

# 3. 开发
npm run dev                        # dev 模式用系统 node + 工作区 node_modules

# 4. 出安装包
npx tauri build                    # → src-tauri/target/release/bundle/nsis/*-setup.exe
```

首次 `tauri build` 会从 GitHub 下载 NSIS 工具链，网络受限时会超时（`timeout: global`）。手动预热缓存：从镜像（如 `https://gh-proxy.com/`）下载下面两个文件，校验 SHA1 后放入 `%LOCALAPPDATA%\tauri\NSIS\`：

| 文件 | SHA1 | 放置位置 |
|---|---|---|
| `nsis-3.11.zip`（binary-releases） | `EF7FF767E5CBD9EDD22ADD3A32C9B8F4500BB10D` | 解压后重命名 `nsis-3.11` → `NSIS/` |
| `nsis_tauri_utils.dll`（v0.5.3） | `75197FEE3C6A814FE035788D1C34EAD39349B860` | `NSIS/Plugins/x86-unicode/` |

## 运行时约定

| 项 | 值 |
|---|---|
| 后端服务端口 | `127.0.0.1:3177–3186`（退让选择） |
| 实例锁端口 | `3176`（仅作互斥 + 聚焦信令，不提供 HTTP） |
| 控制通道端口 | `3175`（GET-only，背景面板的页面↔壳桥接） |
| 应用数据目录 | `%APPDATA%\dsh-desktop-shell-tauri\`（`dsh-home/`、`logs/dsh.log`、`background.*`） |

页面内背景面板的实现：webview 加载的是远程 http 源，Tauri IPC 不可用，壳在 3175 起极简 HTTP 端点（`/bg/state|pick|clear|opacity`，带 `ACAO: *`），页面脚本（Shadow DOM 注入，`on_page_load` + `initialization_script` 双保险）通过 fetch 调用。该脚本本体是独立文件 `src-tauri/assets/shell-panel.js`（编译期 `include_str!` 注入，build.rs 里 `node --check` 校验语法，`__DSH_TOKEN__` 占位符由壳在注入时替换）。

## 已知坑（都是踩过的）

1. **`ELECTRON_RUN_AS_NODE`**：Electron 系 IDE（Trae/VS Code 等）的终端会把这个变量带进子进程环境，spawn 任何 Electron/Node 二进制都会行为异常。壳在 spawn 前显式 `env_remove`。
2. **dsh 不能跑在 Electron-as-Node 下**：`koffi.view()` 创建外置 Buffer 在 Electron 的 V8 沙箱里是致命错误（目录选择器必崩）。dsh 必须用真 Node——这也是本壳内嵌 Node 运行时而不是复用 Electron 的原因。
3. **裁剪 `node_modules` 时不能删 `doc/` 目录**：`yaml` 包的运行时代码在 `dist/doc/`（`composer.js` 引用 `../doc/directives.js`）。`prune-runtime.cjs` 的目录白名单是保守口径，别往里加 `doc/docs/test/tests/spec`。
4. **NSIS 钩子键名是驼峰**：本 CLI 版本（2.11.x）的 schema 用 `installerHooks`，不是文档里的 kebab-case。写错会报 `not valid under any of the schemas`。
5. **装前/卸前必须杀 sidecar**：NSIS 模板只检查主程序进程；`dsh-node.exe` 持有的 DLL（如 libvips）会导致覆写失败（`Error opening file for writing`）。node 改名 `dsh-node.exe` 就是为了让 `nsis-hooks.nsh` 里的 `taskkill /IM` 不误伤系统里其他 node 进程。
6. **单实例检查必须在 spawn 后端之前**：否则二次启动会先拉起一个孤儿后端再去撞单实例锁。用 3176 锁端口在 `run()` 第一行抢占。
7. **release 无控制台**：spawn 子进程要加 `CREATE_NO_WINDOW`（否则弹黑窗）；子进程 stdout/stderr 重定向到 `logs/dsh.log`。
8. **就绪探测要用 HTTP 200，不是 TCP connect**：TCP 监听先于 webserver 插件就绪，页面加载太早会导致客户端插件树半边启动失败（`Failed to load plugins`）。
9. **`initialization_script` 在 `navigate()` 后不保证重跑**：页面注入逻辑必须再用 `on_page_load(Finished)` 兜底。
10. **控制通道必须带 token**：3175 是 loopback HTTP 且无 CORS 限制，不带 token 任何网页都能 `fetch` 进来关应用/偷背景图。token 在启动时生成、注入页面脚本；不可用 Origin 白名单替代（dev 下 splash 的 origin 是随机端口）。
11. **无边框窗口不要提供全屏**：`set_fullscreen` 会换成 `WS_POPUP` 样式，退出后边框缩放失效。最大化已覆盖该场景。
12. **identifier 即升级键**：NSIS 按 `tauri.conf.json` 的 `identifier` 生成安装 GUID，发布后改动会导致旧版无法覆盖升级。当前正式值 `com.qwq123321123.dsh-desktop`，不要再动。
13. **dev 与 release 共用 DSH_HOME**（`%APPDATA%\dsh-desktop-shell-tauri\dsh-home`）：凭证/会话互通是刻意设计（曾经分裂导致旧 key  stranded）；代价是 dev 分支的 bug 可能污染正式数据，开发时留意。

## 升级 dsh 版本的检查清单

1. 改 `resources/dsh-runtime/package.json` 里的 `@deepseek-ai/dsh` 版本，重新 `npm install --omit=dev`
2. 重新跑 `scripts/prune-runtime.cjs`
3. 完整验证一轮：启动（看 `logs/dsh.log` 无 fatal）→ 选工作区（koffi 目录选择器）→ 发一条消息（真对话）→ 设/清背景 → 托盘退出（无孤儿 `dsh-node.exe`）
4. 重出安装包，覆盖安装一次（验证装前杀进程钩子）→ 卸载一次（验证数据目录保留）

dsh 处于开发者预览期，不承诺兼容；升级前看一眼其 [BREAKING 变更](../deepseek-harness/AGENTS.md)。

## 自动化冒烟测试

`scripts/smoke-test.py` 把 13.2 验收清单中可无头自动化的部分固化为一键回归，覆盖：控制通道 token 门禁（无 token 全 403）、端口退让、端口全占失败路径、单实例握手、锁端口被无关程序占用时拒绝启动、退出无孤儿进程、退出后立即重启。

```sh
cd src-tauri && cargo build        # 先构建 dev exe（或 npm run dev 一次）
cd .. && python scripts/smoke-test.py
```

前置条件：工作区已 `npm install`（`node_modules/@deepseek-ai/dsh` 存在）、没有正在运行的应用实例（脚本会前置检查 3175/3176 端口并明确报错）。测试期间应用窗口会短暂弹出数次；脚本只调用只读端点，不写背景/凭证数据（dev/release 共用 DSH_HOME 的已知坑 #13 依旧存在）。

不覆盖的手工项：splash 视觉跳转、背景选择对话框、右键菜单行为、关于对话框、>10MB 背景流畅度、覆盖安装/卸载（仍走 13.2 清单）。

### 壳与 dsh DOM 的耦合点（dsh 改版面时优先检查）

- **设置整页**：`[role="dialog"][aria-modal="true"]:has(nav)` 结构选择器 + fixed 全屏覆盖（CSS Modules 类名带哈希，只能按结构匹配）
- **全局 chrome CSS**：`html { margin-top: 32px; overflow: hidden }` 依赖 dsh 的滚动容器是内部 flex 子元素
- **菜单快捷键**：Ctrl+N / Ctrl+, 按按钮**文本**精确匹配 `新会话`/`设置`，dsh 改文案即失效（将来可换 aria-label）
- **背景面板**：注入的 Shadow DOM 不依赖 dsh，但 `light-dark()` 主题跟随依赖 dsh 在 root 上设置 `color-scheme`

## 目录结构

```
src-tauri/            Rust 壳（src/lib.rs 是全部主逻辑）
  assets/shell-panel.js  注入页面的 UI 脚本（标题栏/菜单/背景面板；编译期注入，node --check 校验）
  resources/          打包资源（node 运行时 + dsh 依赖树，不入库）
  nsis-hooks.nsh      安装器装前/卸前杀 sidecar
assets/               DeepSeek logo SVG
scripts/              gen-icons / gen-splash / prune-runtime / smoke-test / 诊断脚本
dist/                 splash.html（构建产物，可重新生成）
```

## License

MIT。dsh 本体及其依赖的许可见 deepseek-harness 仓库的 THIRD_PARTY_NOTICES。
