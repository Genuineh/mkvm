!macro MYKVM_CLOSE_RUNNING_INSTANCES
  DetailPrint "Closing running mkvm instances..."
  IfFileExists "$INSTDIR\mkvm.exe" 0 +2
    ExecWait '"$INSTDIR\mkvm.exe" --mykvm-quit-existing'
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -Command "$deadline=(Get-Date).AddSeconds(8); while ((Get-Process -Name mkvm -ErrorAction SilentlyContinue) -and ((Get-Date) -lt $deadline)) { Start-Sleep -Milliseconds 200 }; Get-Process -Name mkvm -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue"'
  Sleep 300
!macroend

!macro MYKVM_STOP_INPUT_SERVICE
  DetailPrint "Stopping MKVM input service..."
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -Command "$svc=Get-Service -Name ''MyKVMInputService'' -ErrorAction SilentlyContinue; if ($svc) { Stop-Service -Name ''MyKVMInputService'' -Force -ErrorAction SilentlyContinue; $deadline=(Get-Date).AddSeconds(12); while (((Get-Service -Name ''MyKVMInputService'' -ErrorAction SilentlyContinue).Status -ne ''Stopped'') -and ((Get-Date) -lt $deadline)) { Start-Sleep -Milliseconds 250 } }; Get-Process -Name ''mkvm-input-helper'' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 400"'
!macroend

!macro MYKVM_FREE_INPUT_HELPER
  ; The input helper is run by the LocalSystem MyKVMInputService, which a per-user
  ; (non-elevated) installer cannot stop, so the .exe stays locked and a plain
  ; overwrite fails with "Error opening file for writing". Windows DOES allow
  ; RENAMING a running executable, so move the locked file aside to free its path:
  ; the new helper is then written normally and the service runs it on its next
  ; (re)start/reboot. The left-over copy is removed on the next update (or reboot).
  DetailPrint "Freeing input helper for replacement..."
  Delete /REBOOTOK "$INSTDIR\mkvm-input-helper.exe.old"
  IfFileExists "$INSTDIR\mkvm-input-helper.exe" 0 +2
    Rename "$INSTDIR\mkvm-input-helper.exe" "$INSTDIR\mkvm-input-helper.exe.old"
!macroend

!macro MYKVM_START_INPUT_SERVICE_IF_INSTALLED
  ; Prefer a full restart so the freshly written helper .exe is picked up. If we
  ; lack rights to stop a LocalSystem service (per-user installer), fall back to
  ; just ensuring it runs; the new helper then loads on the next reboot.
  DetailPrint "Restarting MKVM input service if installed..."
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -Command "$svc=Get-Service -Name ''MyKVMInputService'' -ErrorAction SilentlyContinue; if ($svc) { try { Restart-Service -Name ''MyKVMInputService'' -Force -ErrorAction Stop } catch { Start-Service -Name ''MyKVMInputService'' -ErrorAction SilentlyContinue } }"'
!macroend

!macro MYKVM_DELETE_INPUT_SERVICE
  DetailPrint "Removing MKVM input service..."
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -Command "$svc=Get-Service -Name ''MyKVMInputService'' -ErrorAction SilentlyContinue; if ($svc) { Stop-Service -Name ''MyKVMInputService'' -Force -ErrorAction SilentlyContinue }; sc.exe delete MyKVMInputService"'
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro MYKVM_STOP_INPUT_SERVICE
  !insertmacro MYKVM_FREE_INPUT_HELPER
  !insertmacro MYKVM_CLOSE_RUNNING_INSTANCES
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Allow inbound UDP to mkvm.exe so LAN peers can discover and reach this
  ; device. Best-effort: only succeeds when the installer runs elevated.
  DetailPrint "Configuring Windows Defender Firewall for mkvm..."
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="MyKVM (UDP-In)"'
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="MyKVM (UDP-In)" dir=in action=allow program="$INSTDIR\mkvm.exe" protocol=udp profile=any enable=yes'
  !insertmacro MYKVM_START_INPUT_SERVICE_IF_INSTALLED
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro MYKVM_DELETE_INPUT_SERVICE
  !insertmacro MYKVM_CLOSE_RUNNING_INSTANCES
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Remove the firewall rule we added during install.
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="MyKVM (UDP-In)"'
!macroend
