## v0.3.0 — 自动更新 + 按住说话语音输入

### 自动更新（自研）

- 帮助 →「检查更新…」或托盘菜单同名项触发；更新源为本仓库 GitHub Releases。
- 逐段数字比较版本号（x.y.z），取最新带安装器的 release；后台下载到 `%APPDATA%\dsh-desktop-shell-tauri\updates\`。
- 用 GitHub API 的 `digest`（sha256）校验下载完整性——应用无代码签名证书，这是防篡改边界（SmartScreen 弹窗属已知限制）。
- 下载完成后静默安装（`/S`），壳自动退出释放文件锁，装完需手动重新打开。
- 断网/失败时提供「打开下载页」兜底；更新源 API 基址可用环境变量 `DSH_UPDATE_API_BASE` 覆盖。

### 语音输入（按住说话）

- 聊天页右下角新增 🎤 按钮：按住开始听，松开把识别文本插入输入框。
- 走 Windows 系统自带的 WinRT 连续听写（`Windows.Media.SpeechRecognition`），完全离线、无 API key，识别语言跟随系统语音包。
- 识别会话由专用工作线程持有，页面刷新/松开丢失不会漏开麦克风；重复按住会先拆旧会话。
- 前置条件：系统设置 → 隐私 → 麦克风/语音 权限已开启，且装有匹配的语音包。

### 其他

- 冒烟测试覆盖新增控制通道路径（`/win/update/*`、`/speech/*`，无 token 一律 403）。
- 单元测试：版本比较与下载校验 4 项（`cd src-tauri && cargo test`）。
