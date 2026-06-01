!macro MYKVM_CLOSE_RUNNING_INSTANCES
  DetailPrint "Closing running mykvm instances..."
  IfFileExists "$INSTDIR\mykvm.exe" 0 +2
    ExecWait '"$INSTDIR\mykvm.exe" --mykvm-quit-existing'
  nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -Command "$deadline=(Get-Date).AddSeconds(8); while ((Get-Process -Name mykvm -ErrorAction SilentlyContinue) -and ((Get-Date) -lt $deadline)) { Start-Sleep -Milliseconds 200 }; Get-Process -Name mykvm -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue"'
  Sleep 300
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro MYKVM_CLOSE_RUNNING_INSTANCES
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro MYKVM_CLOSE_RUNNING_INSTANCES
!macroend
