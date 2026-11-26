Set-Location -Path "C:\Users\Ree\Documents\GTAVI Port"

# Archive previous run.log if it exists
if (Test-Path "run.log") {
    $timestamp = (Get-Item "run.log").LastWriteTime.ToString("yyyyMMdd_HHmmss")
    Copy-Item "run.log" "run_$timestamp.log" -Force
}

# Wipe ALL existing VICTORY_* env vars from this shell so nothing persists
Get-ChildItem Env:VICTORY_* -ErrorAction SilentlyContinue | Remove-Item

# Clean 1080p Series S baseline (8 GB VRAM / 16 GB RAM)
$env:VICTORY_DISPLAY_MODE = "1920x1080"
$env:VICTORY_MEMORY_MAP = "0"
$env:VICTORY_TEXTURE_BUDGET_MB = "1536"
$env:VICTORY_HEAP_TEXTURES_MS = "16"
$env:VICTORY_CARD_BUFFERS_MIN_MB = "1"
$env:VICTORY_CARD_BUFFERS_MB = "1024"
$env:VICTORY_PIPELINE_WAIT_MS = "500"
$env:VICTORY_JOIN_COPIES = "1"
$env:VICTORY_CARD_REHASH_RUNS = "1"

# New CPU & rendering flags (Sections 10.22-10.23)
# Note: WIDE_REFRESH, WIDE_TEXTURES, WIDE_BACK, RELAY, PAGE_PARTS,
# TEXTURE_BACK, and HELPER_OWN are now ON by default in this build.
$env:VICTORY_WIDE_MERGE = "1"
$env:VICTORY_AMBIENT = "0.8"
$env:VICTORY_AMBIENT_AIR = "1.5"

# First-run shader re-translation (comment this out after 1 run)
#$env:VICTORY_SHADER_FRESH = "1"

# Fallback: uncomment if you hit a 0x887A GPU device-removed crash
#$env:VICTORY_RELAY = "0"

Write-Host "Active VICTORY_* flags:"
Get-ChildItem Env:VICTORY_* | Format-Table Name, Value -AutoSize

Write-Host "Starting Grand Theft Auto VI via VIctory (Series S, 1080p)..."
cmd /c '".\target\release\victory.exe" run "H:\GTA VI\game_scarlett_master.exe" --execute > "run.log" 2>&1'

Write-Host "`nRun completed. Comparing runs..."
powershell -ExecutionPolicy Bypass -File .\tools\compare-runs.ps1