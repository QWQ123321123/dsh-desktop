# Creates a GitHub Release for dsh-desktop and uploads the NSIS installer.
# Auth: reuses git's stored github.com credential (no token on disk/CLI args).
# Usage: powershell -File scripts/release.ps1 [-Tag v0.3.0] [-Notes "..."]

param(
  [string]$Tag = 'v0.2.1',
  [string]$Repo = 'QWQ123321123/dsh-desktop',
  [string]$Notes = '',
  [string]$NotesFile = ''
)

$ErrorActionPreference = 'Stop'
$ver = $Tag.TrimStart('v')
$repo = $Repo
$tag = $Tag
$asset = "D:\deepseek-desktop\desktop-shell-tauri\src-tauri\target\release\bundle\nsis\DeepSeek Harness (Tauri)_${ver}_x64-setup.exe"
$assetName = "dsh-desktop_${ver}_x64-setup.exe"

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
  name             = "dsh-desktop $tag"
  body             = $(if ($NotesFile) { [System.IO.File]::ReadAllText((Resolve-Path $NotesFile).Path) } elseif ($Notes) { $Notes } else { "dsh-desktop $tag — Windows x64 安装包（NSIS）。" })
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





