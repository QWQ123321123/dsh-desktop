; Pre-install hook: stop the shell's sidecar tree before overwriting files.
; The tauri NSIS template only checks the main exe; our dsh server runs as
; `dsh-node.exe` (renamed stock Node) with worker children, holding DLLs open.
; The distinctive binary name makes taskkill /IM safe (no stock `node.exe` clash).
!macro NSIS_HOOK_PREINSTALL
  nsExec::ExecToLog 'taskkill /IM dsh-node.exe /F /T'
!macroend

; Same before uninstall: the uninstaller also rewrites/deletes the install dir.
!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog 'taskkill /IM dsh-node.exe /F /T'
!macroend
