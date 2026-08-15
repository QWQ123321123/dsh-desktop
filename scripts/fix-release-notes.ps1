# Fixes the v0.1.0 release notes: PS5.1 sends string bodies as ASCII,
# which mangled the Chinese text to '?'. Resend as explicit UTF-8 bytes.

$ErrorActionPreference = 'Stop'
$repo = 'QWQ123321123/dsh-desktop'
$tag = 'v0.1.0'

$cred = "protocol=https`nhost=github.com`n" | git credential fill
$token = ($cred -split "`n" | Where-Object { $_ -like 'password=*' }) -replace '^password=', ''
$headers = @{
  Authorization  = "Bearer $token"
  Accept         = 'application/vnd.github+json'
  'User-Agent'   = 'dsh-desktop-release-script'
}

$release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/tags/$tag" -Headers $headers

$notes = @'
首个可用版本。Windows x64 安装包（NSIS，约 45MB）。

- Tauri 2 壳 + 内嵌 Node 运行时 + dsh web 后端
- 启动动画 / 托盘常驻 / 单实例 / 端口退让 / 页面内背景自定义
- 卸载默认保留用户数据（%APPDATA%\dsh-desktop-shell-tauri）
'@

$json = @{ body = $notes } | ConvertTo-Json
$bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
Invoke-RestMethod -Method Patch -Uri "https://api.github.com/repos/$repo/releases/$($release.id)" -Headers $headers -Body $bytes -ContentType 'application/json; charset=utf-8' | Out-Null
Write-Output 'release notes fixed'

