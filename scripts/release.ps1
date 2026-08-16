# Creates a GitHub Release for dsh-desktop and uploads the NSIS installer.
# Auth: reuses git's stored github.com credential (no token on disk/CLI args).

$ErrorActionPreference = 'Stop'
$repo = 'QWQ123321123/dsh-desktop'
$tag = 'v0.2.0'
$asset = "D:\deepseek-desktop\desktop-shell-tauri\src-tauri\target\release\bundle\nsis\DeepSeek Harness (Tauri)_0.2.0_x64-setup.exe"
$assetName = 'dsh-desktop_0.2.0_x64-setup.exe'

$cred = "protocol=https`nhost=github.com`n" | git credential fill
$token = ($cred -split "`n" | Where-Object { $_ -like 'password=*' }) -replace '^password=', ''
if (-not $token) { throw 'no stored github credential' }
$headers = @{
  Authorization = "Bearer $token"
  Accept        = 'application/vnd.github+json'
  'X-GitHub-Api-Version' = '2022-11-28'
  'User-Agent'  = 'dsh-desktop-release-script'
}

$body = @{
  tag_name         = $tag
  target_commitish = 'main'
  name             = 'dsh-desktop v0.2.0'
  body             = "v0.2.0 — 类 Codex 桌面形态。Windows x64 安装包（NSIS）。`n`n- 无边框窗口 + 自定义标题栏（文件/编辑/视图/帮助菜单、拖动、窗口控件）`n- 设置从弹窗变为整页`n- 页面切换与按钮过渡动画`n- 背景面板/标题栏跟随 dsh 明暗主题`n- 禁用浏览器右键菜单；splash 卡死自愈`n- dev 与正式版共用 DSH_HOME（凭证/会话互通）`n- 卸载默认保留用户数据（%APPDATA%\dsh-desktop-shell-tauri）"
  draft            = $false
  prerelease       = $true
} | ConvertTo-Json

# PS5.1 sends string bodies as ASCII — encode explicitly or CJK text breaks.
$bodyBytes = [System.Text.Encoding]::UTF8.GetBytes($body)
$release = Invoke-RestMethod -Method Post -Uri "https://api.github.com/repos/$repo/releases" -Headers $headers -Body $bodyBytes -ContentType 'application/json; charset=utf-8'
Write-Output "release created: $($release.html_url)"

$uploadUrl = "https://uploads.github.com/repos/$repo/releases/$($release.id)/assets?name=$assetName"
$bytes = [System.IO.File]::ReadAllBytes($asset)
$res = Invoke-RestMethod -Method Post -Uri $uploadUrl -Headers ($headers + @{ 'Content-Type' = 'application/octet-stream' }) -Body $bytes -TimeoutSec 600
Write-Output "asset uploaded: $($res.browser_download_url) ($([math]::Round($res.size/1MB,1))MB)"


