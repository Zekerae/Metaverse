# VIctory / GTAVI Port — Workspace & Build Report

Generated: 2026-11-25  
Workspace: `C:\Users\Ree\Documents\GTAVI Port`

## 1. Git & Baseline State
- **Remote (`origin`)**: `https://github.com/Zekerae/Metaverse.git`
- **Current Branch / Commit**: `ztrx-baseline` at `90b529f` (`ztrx: own README and a launcher for normal play`, fetched from `https://github.com/Zettrox/ztrx.git` `main`)
- **Working Tree (`git status -s`)**: Clean on all tracked files; untracked local helpers and logs only:
  - `run-game.ps1`
  - `tools/` (`compare-runs.ps1`)
  - `run.log` and archived `run_*.log` files
- **Stashes (`git stash list`)**:
  - `stash@{0}: On main: pre-ztrx-baseline` (11 files, `+1115/-63`: prior local edits across `Cargo.lock`, `apps/victory/src/run.rs`, `crates/victory-game/`, `crates/victory-gpu/`, `crates/victory-hle/`, and `crates/victory-loader/`)

## 2. Key Crates & Workspace Layout
From `Cargo.toml` (`members = ["crates/*", "apps/*", "xtask"]`, `edition = "2024"`, `rust-version = "1.88"`):
- **Internal Crates**: `victory-core`, `victory-pe`, `victory-loader`, `victory-hle`, `victory-gpu`, `victory-shader`, `victory-formats`, `victory-pipeline`, `victory-game`
- **Modified/Added relative to `ztrx-baseline` (`90b529f`)**: None in tracked source files (all prior modifications are saved in `stash@{0}`). Untracked additions in the workspace root are `run-game.ps1` and `tools/compare-runs.ps1`.

## 3. Build & Launch Configuration
- **Target Binary**: `C:\Users\Ree\Documents\GTAVI Port\target\release\victory.exe` (`56,406,393` bytes, built `2026-11-25 11:51:56`)
- **Title Config (`victory.toml`)**: `game_dir = 'H:\GTA VI'`
- **Launcher (`run-game.ps1`)**:
  ```powershell
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
  ```
- **Active `VICTORY_*` Flags & Cache**: No custom `VICTORY_*` overrides are set in `run-game.ps1` (`VICTORY_CARD_REHASH_RUNS=0`, `VICTORY_JOIN_COPIES=0`, `VICTORY_PIPELINE_WAIT_MS=500` by default). Translated image cache is under `%LOCALAPPDATA%\victory\cache\images` and the host pipeline library was saved at `bytes=13554860`.

## 4. Latest Run Summary (`run.log`)
- **File**: `run.log` (`2,141,822` bytes, `2026-11-25 11:59:16`)
- **Highest Frame Reached**: `1461`
- **Exceptions / Crashes**: `0` (all `0x80070057` `E_INVALIDARG` entries were `WARN` logs; exited/stopped cleanly after saving `host pipeline library` with `early=1582 made=407 loaded=55 started_early=31`).
- **Performance & Notable Warnings (`47` total `WARN` lines)**:
  - Frame rate dropped to `0.6–1.4 fps` (`712.9–1670.7 ms/frame`) during heavy copy, texture refresh, and shader compile activity (`318.9 ms/frame` texture refreshes, `254.8 ms` waiting for remapped title memory, `52` buffer copies/frame).
  - `victory_hle::modules::dstorage`: `dstorage: a destination's physical pages could not be written (unknown page or a host failure); its request fails`
  - `victory_gpu::pipeline::host` / `draw`: `host pipeline refused error=The parameter is incorrect. (0x80070057)` and `a dispatch without a host pipeline dropped`
  - `victory_gpu::descriptor::host`: `unordered buffer view the host would refuse left null`, `placed unordered texture view not mirrored dimension=3`, `constant buffer not 256-byte aligned on the host`
