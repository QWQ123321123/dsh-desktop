$roots = @(
  'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
  'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
  'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall'
)
foreach ($root in $roots) {
  Get-ChildItem $root -ErrorAction SilentlyContinue | ForEach-Object {
    $p = Get-ItemProperty $_.PSPath
    if ($p.DisplayName -match 'DeepSeek|Harness|dsh') {
      Write-Output "ROOT: $root"
      Write-Output "KEY: $($_.PSChildName)"
      Write-Output "NAME: $($p.DisplayName)"
      Write-Output "LOC: $($p.InstallLocation)"
      Write-Output "UNINSTALL: $($p.UninstallString)"
      Write-Output "---"
    }
  }
}
