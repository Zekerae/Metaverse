Set-Location -Path "C:\Users\Ree\Documents\GTAVI Port"

# Archive previous run.log if it exists
if (Test-Path "run.log") {
    $timestamp = (Get-Item "run.log").LastWriteTime.ToString("yyyyMMdd_HHmmss")
    Copy-Item "run.log" "run_$timestamp.log" -Force
}

Write-Host "Starting Grand Theft Auto VI via VIctory..."
& ".\target\release\victory.exe" run "H:\GTA VI\game_scarlett_master.exe" --execute *>&1 | Tee-Object -FilePath "run.log"

Write-Host "`nRun completed. Comparing runs..."
powershell -ExecutionPolicy Bypass -File .\tools\compare-runs.ps1
