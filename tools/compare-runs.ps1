# Compares VIctory run logs
$files = Get-ChildItem -Path . -Filter "run*.log" -File -ErrorAction SilentlyContinue | Sort-Object LastWriteTime
if (-not $files) {
    Write-Host "No log files found matching run*.log"
    exit
}

Write-Host "=================================================="
Write-Host "           VICTORY RUN LOG COMPARISON             "
Write-Host "=================================================="
Write-Host "Total log runs recorded: $($files.Count)`n"

$recent = $files | Select-Object -Last 5
$i = 1
foreach ($f in $recent) {
    $maxFrame = 0
    $dstorageWarns = @()
    $recoveredCount = 0
    $exceptions = @()
    $callstack = @()

    try {
        $stream = [System.IO.File]::Open($f.FullName, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
        $reader = New-Object System.IO.StreamReader($stream, $true)
        while (-not $reader.EndOfStream) {
            $raw = $reader.ReadLine()
            $clean = $raw -replace '\x1b\[[0-9;]*[a-zA-Z]', ''
            if ($clean -match "frame\s*=\s*(\d+)") {
                $frame = [int]$matches[1]
                if ($frame -gt $maxFrame) { $maxFrame = $frame }
            }
            if ($clean.Contains("dstorage:") -and $clean.Contains("WARN")) {
                $dstorageWarns += $clean.Trim()
            }
            if ($clean.Contains("recovered from invalid")) {
                $recoveredCount++
            }
            if ($clean -match "exception 0x") {
                $exceptions += $clean.Trim()
            }
            if ($clean -match "#\d\s+victory-image") {
                $callstack += $clean.Trim()
            }
        }
        $reader.Close()
        $stream.Close()
    } catch {
        # skip if inaccessible
    }

    Write-Host "[Run #$i] $($f.Name) ($([math]::Round($f.Length / 1KB)) KB) - $($f.LastWriteTime.ToString('yyyy-MM-dd HH:mm:ss'))"
    Write-Host "  * Highest Frame Reached: $maxFrame"
    if ($recoveredCount -gt 0) {
        Write-Host "  * Recovered Invalid Calls: $recoveredCount"
    }
    if ($dstorageWarns.Count -gt 0) {
        Write-Host "  * DirectStorage Warnings ($($dstorageWarns.Count)):"
        $dstorageWarns | Select-Object -Last 2 | ForEach-Object {
            $msg = if ($_.Length -gt 110) { $_.Substring(0, 110) + "..." } else { $_ }
            Write-Host "      - $msg"
        }
    } else {
        Write-Host "  * DirectStorage Warnings: None"
    }

    if ($exceptions.Count -gt 0) {
        Write-Host "  * Crash Exception:"
        $exceptions | ForEach-Object { Write-Host "      ! $_" }
    } else {
        Write-Host "  * Crash Exception: None (Clean exit)"
    }

    if ($callstack.Count -gt 0) {
        Write-Host "  * Top Call Stack:"
        $callstack | Select-Object -First 4 | ForEach-Object { Write-Host "      $_" }
    }
    Write-Host ""
    $i++
}
Write-Host "=================================================="
