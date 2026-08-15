$ports = 3177..3186
foreach ($port in $ports) {
  $conn = Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($conn) {
    $proc = Get-CimInstance Win32_Process -Filter "ProcessId=$($conn.OwningProcess)"
    Write-Output "port $port -> PID $($conn.OwningProcess): $($proc.ExecutablePath)"
  }
}
