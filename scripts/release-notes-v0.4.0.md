## v0.4.0 — 插件市场

帮助 →「插件市场」：管理 dsh 的 profile 插件（已安装列表、官方目录、按包名安装）。

- **安装/卸载**直接调用上游 `dsh plugin --profile web add|remove`（pnpm 转发器），插件是声明 `dsh.bundle.patch` 的 npm 包；装完/卸完提供「立即重启」使插件生效。
- **官方目录**：仓库 `catalog.json` 为收录清单（GitHub 托管拉取 + 本地缓存），提交 PR 即可收录插件；当前为空，机制先行。
- **信任边界**：仅接受 npm 注册表包名（白名单校验，防命令注入）；目录外的包安装前弹出「来源未验证」警告。
- **一键安装 pnpm**：dsh 插件管理依赖 pnpm，市场内可后台任务自动安装（`npm i -g pnpm`）。
- 控制通道 `/plugin/list|catalog|status|install|remove|ensure-pnpm`，安装/卸载为后台任务 + 状态轮询。

### 其他

- 新增 `/win/restart`（重启壳 + 连带重启 dsh 子进程）。
- 冒烟测试新增 7 个插件相关门禁路径；单元测试 9 项（含包名白名单、pnpm list 解析、目录解析）。
