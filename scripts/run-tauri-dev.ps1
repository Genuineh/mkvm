$env:Path = "$env:USERPROFILE\.cargo\bin;" + $env:Path
$env:CARGO_TARGET_DIR = "$env:LOCALAPPDATA\CargoTarget\mkvm"

Write-Host "Starting mkvm Tauri dev environment..."
Write-Host "CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR"

npm.cmd run tauri:dev
exit $LASTEXITCODE
