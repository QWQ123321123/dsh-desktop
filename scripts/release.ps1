# Creates a GitHub Release for dsh-desktop and uploads the NSIS installer.
# Auth: reuses git's stored github.com credential (no token on disk/CLI args).

$ErrorActionPreference = 'Stop'
$repo = 'QWQ123321123/dsh-desktop'
$tag = 'v0.1.0'
$asset = "D:\deepseek-desktop\desktop-shell-tauri\src-tauri\target\release\bundle\nsis\DeepSeek Harness (Tauri)_0.1.0_x64-setup.exe"
$assetName = 'dsh-desktop_0.1.0_x64-setup.exe'

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
  name             = 'dsh-desktop v0.1.0'
  body             = "首个可用版本。Windows x64 安装包（NSIS）。`n`n- Tauri 2 壳 + 内嵌 Node 运行时 + dsh web 后端`n- 启动动画 / 托盘常驻 / 单实例 / 端口退让 / 页面内背景自定义`n- 卸载默认保留用户数据（%APPDATA%\dsh-desktop-shell-tauri）"
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

