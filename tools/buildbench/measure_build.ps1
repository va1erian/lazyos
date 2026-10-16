<#
.SYNOPSIS
    Time a complete LazyOS build from a clean tree (to compare CPUs).

.DESCRIPTION
    Each run deletes every build output (target\, xui-app\target\,
    rhai-host\target\), then times the same command a user runs:

        python tools\run_demo.py --desktop --build-only --yes --reset-os

    which builds rhai, the xui desktop apps and their core packages, the
    kernel, the services and the disk image. Downloads are kept out of the
    measurement: crates are fetched, the musl target added and BusyBox (a
    Docker build) made before the clock starts, and BusyBox and the /home and
    data disks are set aside during the clean and put back afterwards.

    Every output line is timestamped in a log, the time is split into phases
    (setup, rhai, xui apps, cargo image build), and one row per run is
    appended to a CSV that outlives the clean, so runs on the old and the new
    CPU can be compared side by side. Build the same commit on both.

    The OS volume in target\lazyos.img is recreated: installed apps,
    settings and logs in it are lost (target\home.img is kept).

.EXAMPLE
    .\tools\buildbench\measure_build.ps1 -Label "Ryzen 5 3600" -Runs 3

.EXAMPLE
    .\tools\buildbench\measure_build.ps1 -Label "after upgrade" -Profile console -Quiet
#>
[CmdletBinding()]
param(
    # A name for this machine or configuration, written to the CSV.
    [string]$Label = "",
    # How many clean builds to time (each one starts from scratch).
    [ValidateRange(1, 50)][int]$Runs = 1,
    # desktop: the default desktop image; console: the kernel and services only.
    [ValidateSet("desktop", "console")][string]$Profile = "desktop",
    # Build the release profile of the image (run_demo.py --release).
    [switch]$Release,
    # Limit cargo's parallel jobs (CARGO_BUILD_JOBS); 0 = cargo's default.
    [ValidateRange(0, 1024)][int]$Jobs = 0,
    # Also rebuild BusyBox (needs Docker; adds a download to the timing).
    [switch]$IncludeBusybox,
    # Skip `cargo fetch` before timing (offline, crates already cached).
    [switch]$SkipFetch,
    # Show only phase changes on the console (the log still has every line).
    [switch]$Quiet,
    # Do not ask before deleting the build outputs.
    [switch]$Yes,
    # Where results go; kept outside target\ so a clean does not erase it.
    [string]$ResultsDir = (Join-Path $env:LOCALAPPDATA "lazyos-build-bench")
)

$ErrorActionPreference = "Stop"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$CleanDirs = @("target", "xui-app\target", "rhai-host\target")
# Kept across the clean: not part of a build, or a download/Docker step.
$KeepFiles = @("target\home.img", "target\data.img", "target\dbgd.key")
$BusyboxDir = "target\abi\busybox"
$StashDir = Join-Path $Root "shots\.buildbench-keep"

function Say([string]$Text, [string]$Color = "Cyan") {
    Write-Host $Text -ForegroundColor $Color
}

function Fail([string]$Text) {
    Write-Host "error: $Text" -ForegroundColor Red
    exit 1
}

function Format-Duration([double]$Seconds) {
    $span = [TimeSpan]::FromSeconds($Seconds)
    "{0:00}:{1:00}:{2:00}.{3}" -f [int][math]::Floor($span.TotalHours), $span.Minutes, $span.Seconds,
        [int][math]::Floor($span.Milliseconds / 100)
}

function Get-MachineInfo {
    $cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
    $sockets = @(Get-CimInstance Win32_Processor).Count
    $ram = (Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory
    $plan = (powercfg /getactivescheme) -replace '^.*\((.*)\)\s*$', '$1'
    $rustc = (& rustc --version 2>$null)
    $commit = (& git -C $Root rev-parse --short HEAD 2>$null)
    $dirty = (& git -C $Root status --porcelain --untracked-files=no 2>$null)
    if ($dirty) { $commit = "$commit+dirty" }
    [pscustomobject]@{
        Cpu      = $cpu.Name.Trim()
        Cores    = $cpu.NumberOfCores * $sockets
        Threads  = $cpu.NumberOfLogicalProcessors * $sockets
        MaxMHz   = $cpu.MaxClockSpeed
        RamGB    = [math]::Round($ram / 1GB, 1)
        Power    = "$plan".Trim()
        Rustc    = "$rustc".Trim()
        Commit   = "$commit".Trim()
    }
}

function Assert-Ready {
    foreach ($tool in @("python", "cargo", "rustup", "git")) {
        if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { Fail "$tool is not on PATH" }
    }
    if (Get-Process -Name "qemu-system-*" -ErrorAction SilentlyContinue) {
        Fail "QEMU is running (it may hold target\lazyos.img open); close it first"
    }
    $busy = Get-Process -Name "cargo", "rustc", "rust-analyzer", "link", "lld" -ErrorAction SilentlyContinue
    if ($busy) {
        $names = ($busy | Select-Object -ExpandProperty ProcessName -Unique) -join ", "
        Say "warning: $names running: it competes for the CPU and may lock files in target\" Yellow
    }
    if ($env:RUSTC_WRAPPER -or $env:CARGO_BUILD_RUSTC_WRAPPER) {
        Say "note: RUSTC_WRAPPER (sccache?) is disabled for the measurement" Yellow
    }
    if ($env:CARGO_TARGET_DIR) {
        Say "note: CARGO_TARGET_DIR is ignored (the build scripts expect .\target)" Yellow
    }
}

function Invoke-Untimed([string]$What, [scriptblock]$Action) {
    Say "  $What"
    & $Action
    if ($LASTEXITCODE -ne 0) { Fail "$What failed (exit $LASTEXITCODE)" }
}

function Initialize-Downloads {
    Say "Preparing (not timed): toolchain, crates, BusyBox"
    Push-Location $Root
    try {
        Invoke-Untimed "rustup: toolchain and the musl target" {
            rustup target add x86_64-unknown-linux-musl | Out-Null
        }
        if (-not $SkipFetch) {
            Invoke-Untimed "cargo fetch (root)" { cargo fetch --quiet }
            Invoke-Untimed "cargo fetch (xui-app)" { cargo fetch --quiet --manifest-path xui-app\Cargo.toml }
            Invoke-Untimed "cargo fetch (rhai-host)" { cargo fetch --quiet --manifest-path rhai-host\Cargo.toml }
        }
        if (-not $IncludeBusybox) {
            $probe = "import sys; sys.path.insert(0, 'tools/abi'); import busybox; " +
                     "p = busybox.ensure_busybox(); print(p or ''); sys.exit(0 if p else 1)"
            Invoke-Untimed "BusyBox (cached, or built once in Docker)" { python -c $probe | Out-Null }
        }
    } finally {
        Pop-Location
    }
}

function Save-Kept {
    if (Test-Path $StashDir) { Fail "$StashDir exists (an interrupted run?): move its files back into target\ first" }
    New-Item -ItemType Directory -Path $StashDir | Out-Null
    foreach ($rel in $KeepFiles) {
        $path = Join-Path $Root $rel
        if (Test-Path $path -PathType Leaf) { Move-Item $path (Join-Path $StashDir (Split-Path $rel -Leaf)) }
    }
    $bb = Join-Path $Root $BusyboxDir
    if (-not $IncludeBusybox -and (Test-Path $bb)) {
        $dest = Join-Path $StashDir "busybox"
        New-Item -ItemType Directory -Path $dest | Out-Null
        Get-ChildItem $bb -File | Move-Item -Destination $dest
    }
}

function Restore-Kept {
    if (-not (Test-Path $StashDir)) { return }
    New-Item -ItemType Directory -Force -Path (Join-Path $Root "target") | Out-Null
    foreach ($rel in $KeepFiles) {
        $saved = Join-Path $StashDir (Split-Path $rel -Leaf)
        if (Test-Path $saved) { Move-Item -Force $saved (Join-Path $Root $rel) }
    }
    $savedBb = Join-Path $StashDir "busybox"
    if (Test-Path $savedBb) {
        $bb = Join-Path $Root $BusyboxDir
        New-Item -ItemType Directory -Force -Path $bb | Out-Null
        Get-ChildItem $savedBb -File | Move-Item -Force -Destination $bb
    }
    Remove-Item -Recurse -Force $StashDir
}

function Clear-BuildOutputs {
    Save-Kept
    try {
        foreach ($rel in $CleanDirs) {
            $path = Join-Path $Root $rel
            if (Test-Path $path) {
                Say "  deleting $rel"
                cmd /c rd /s /q $path | Out-Null
                if (Test-Path $path) { Fail "could not delete $path (a file is in use?)" }
            }
        }
    } finally {
        # The cached BusyBox and the disks go back before the clock starts.
        Restore-Kept
    }
}

# run_demo.py announces rhai and the xui apps; the image's `cargo build` starts
# with the first cargo status line after them. tools\xui\build.py streams its
# own cargo output, so in that phase only a line after its `Finished` counts.
function Get-Phase([string]$Line, $State) {
    $current = $State.Phase
    if ($Line -match '^building rhai') { return "rhai" }
    if ($Line -match '^building the xui apps') { $State.XuiDone = $false; return "xui" }
    if ($current -eq "image") { return $current }
    if ($current -eq "xui") {
        if ($Line -match '^\s+Finished\s') { $State.XuiDone = $true; return $current }
        if (-not $State.XuiDone) { return $current }
    }
    if ($Line -match '^\s+(Compiling|Locking|Updating|Blocking|Building|Checking)\s') { return "image" }
    return $current
}

function Invoke-TimedBuild([int]$Run, [string]$LogFile) {
    $demoArgs = @("--build-only", "--yes", "--reset-os")
    if ($Profile -eq "desktop") { $demoArgs += "--desktop" }
    if ($Release) { $demoArgs += "--release" }
    $command = "python tools\run_demo.py $($demoArgs -join ' ') 2>&1"

    $state = @{ Phase = "setup"; XuiDone = $false; Marks = [ordered]@{ setup = 0.0 } }
    $clock = [Diagnostics.Stopwatch]::StartNew()
    Push-Location $Root
    $savedEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        cmd /c $command | ForEach-Object {
            $t = $clock.Elapsed.TotalSeconds
            $line = "$_"
            $next = Get-Phase $line $state
            if ($next -ne $state.Phase) {
                $state.Phase = $next
                $state.Marks[$next] = $t
                if ($Quiet) { Say ("  [{0}] phase: {1}" -f (Format-Duration $t), $next) DarkCyan }
            }
            $stamped = "[{0}] {1}" -f (Format-Duration $t), $line
            Add-Content -Path $LogFile -Value $stamped -Encoding UTF8
            if (-not $Quiet) { Write-Host $stamped }
        }
        $exit = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $savedEap
        Pop-Location
    }
    $clock.Stop()
    $total = $clock.Elapsed.TotalSeconds

    # Each phase lasts until the next one starts.
    $phases = @{ setup = 0.0; rhai = 0.0; xui = 0.0; image = 0.0 }
    $keys = @($state.Marks.Keys)
    for ($i = 0; $i -lt $keys.Count; $i++) {
        $end = if ($i + 1 -lt $keys.Count) { $state.Marks[$keys[$i + 1]] } else { $total }
        $phases[$keys[$i]] += $end - $state.Marks[$keys[$i]]
    }
    [pscustomobject]@{ Run = $Run; Exit = $exit; Total = $total; Phases = $phases }
}

function Add-ResultRow($Info, $Result, [string]$CsvFile) {
    $row = [pscustomobject][ordered]@{
        date       = (Get-Date).ToString("yyyy-MM-dd HH:mm:ss")
        label      = $Label
        cpu        = $Info.Cpu
        cores      = $Info.Cores
        threads    = $Info.Threads
        ram_gb     = $Info.RamGB
        power_plan = $Info.Power
        commit     = $Info.Commit
        rustc      = $Info.Rustc
        profile    = $Profile + $(if ($Release) { "-release" } else { "" })
        jobs       = $(if ($Jobs -gt 0) { $Jobs } else { "default" })
        run        = $Result.Run
        exit_code  = $Result.Exit
        total_s    = [math]::Round($Result.Total, 1)
        setup_s    = [math]::Round($Result.Phases.setup, 1)
        rhai_s     = [math]::Round($Result.Phases.rhai, 1)
        xui_s      = [math]::Round($Result.Phases.xui, 1)
        image_s    = [math]::Round($Result.Phases.image, 1)
    }
    $row | Export-Csv -Path $CsvFile -Append -NoTypeInformation -Encoding UTF8
}

function Show-Summary($Results, [string]$CsvFile) {
    Say "`nResults" Green
    foreach ($r in $Results) {
        $p = $r.Phases
        $status = if ($r.Exit -eq 0) { "ok" } else { "FAILED (exit $($r.Exit))" }
        "  run {0}: total {1}  (setup {2}, rhai {3}, xui apps {4}, cargo image {5})  {6}" -f $r.Run,
            (Format-Duration $r.Total), (Format-Duration $p.setup), (Format-Duration $p.rhai),
            (Format-Duration $p.xui), (Format-Duration $p.image), $status | Write-Host
    }
    $ok = @($Results | Where-Object { $_.Exit -eq 0 } | ForEach-Object { $_.Total } | Sort-Object)
    if ($ok.Count -gt 1) {
        $median = if ($ok.Count % 2) { $ok[[int][math]::Floor($ok.Count / 2)] }
                  else { ($ok[$ok.Count / 2 - 1] + $ok[$ok.Count / 2]) / 2 }
        $mean = ($ok | Measure-Object -Average).Average
        "  best {0}  median {1}  mean {2}" -f (Format-Duration $ok[0]), (Format-Duration $median),
            (Format-Duration $mean) | Write-Host
    }
    Say "`nAll runs so far ($CsvFile):" Green
    Import-Csv $CsvFile | Format-Table date, label, cpu, threads, commit, profile, run, exit_code, total_s, rhai_s, xui_s, image_s -AutoSize |
        Out-String | Write-Host
}

# --- main ------------------------------------------------------------------

Assert-Ready
$info = Get-MachineInfo
Say "LazyOS clean-build benchmark" Green
"  CPU      {0} ({1} cores / {2} threads, {3} MHz)" -f $info.Cpu, $info.Cores, $info.Threads, $info.MaxMHz | Write-Host
"  RAM      {0} GB    power plan: {1}" -f $info.RamGB, $info.Power | Write-Host
"  commit   {0}    {1}" -f $info.Commit, $info.Rustc | Write-Host
"  profile  {0}{1}, {2} run(s), jobs: {3}" -f $Profile, $(if ($Release) { " (release)" } else { "" }), $Runs,
    $(if ($Jobs -gt 0) { $Jobs } else { "default" }) | Write-Host

if (-not $Yes) {
    Write-Host "`nThis deletes target\, xui-app\target\ and rhai-host\target\ before each run;" -ForegroundColor Yellow
    Write-Host "the OS volume in target\lazyos.img is recreated (home.img is kept)." -ForegroundColor Yellow
    if ((Read-Host "Proceed? [y/N]") -notmatch '^(y|yes)$') { exit 1 }
}

New-Item -ItemType Directory -Force -Path $ResultsDir | Out-Null
$csv = Join-Path $ResultsDir "results.csv"
$stamp = (Get-Date).ToString("yyyyMMdd-HHmmss")

# Child processes inherit these; restored at the end.
$savedEnv = @{}
foreach ($name in @("RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER", "CARGO_TARGET_DIR", "CARGO_BUILD_JOBS",
                    "CARGO_TERM_COLOR", "CARGO_TERM_PROGRESS_WHEN", "PYTHONUNBUFFERED")) {
    $savedEnv[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
}
try {
    $env:RUSTC_WRAPPER = $null
    $env:CARGO_BUILD_RUSTC_WRAPPER = $null
    $env:CARGO_TARGET_DIR = $null
    $env:CARGO_TERM_COLOR = "never"
    $env:CARGO_TERM_PROGRESS_WHEN = "never"
    $env:PYTHONUNBUFFERED = "1"
    $env:CARGO_BUILD_JOBS = $(if ($Jobs -gt 0) { "$Jobs" } else { $null })

    Initialize-Downloads
    $results = @()
    for ($run = 1; $run -le $Runs; $run++) {
        Say "`nRun $run of $Runs" Green
        Say "Cleaning"
        Clear-BuildOutputs
        $log = Join-Path $ResultsDir "build-$stamp-run$run.log"
        Say "Building (timed; log: $log)"
        $result = Invoke-TimedBuild $run $log
        $results += $result
        Add-ResultRow $info $result $csv
        "  total {0}" -f (Format-Duration $result.Total) | Write-Host
        if ($result.Exit -ne 0) {
            Say "the build failed; see $log" Red
            break
        }
    }
    Show-Summary $results $csv
    if (@($results | Where-Object { $_.Exit -ne 0 }).Count) { exit 1 }
} finally {
    foreach ($name in $savedEnv.Keys) {
        [Environment]::SetEnvironmentVariable($name, $savedEnv[$name], "Process")
    }
}
