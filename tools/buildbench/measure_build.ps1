<#
.SYNOPSIS
    Time a complete LazyOS build from a clean tree (to compare CPUs).

.DESCRIPTION
    Before the first run the existing build directories (target\,
    xui-app\target\, rhai-host\target\, vendor\bootloader\target\) are set
    aside by renaming them; nothing in them is deleted, and they are put back
    when the script ends. Each run then starts from empty directories and
    times the same command a user runs:

        python tools\run_demo.py --desktop --build-only --yes --reset-os

    which builds rhai, the xui desktop apps and their core packages, the
    bootloader, the kernel, the services and the disk image. Downloads stay
    out of the measurement: the musl target, the crates and BusyBox (a Docker
    build) are fetched before the clock starts, and the timed build runs with
    CARGO_NET_OFFLINE so a stray download fails instead of skewing the time.

    Each line of output is timestamped in a log, the time is split into
    phases (setup, rhai, xui apps, cargo image build) and one row per run is
    appended to a CSV kept outside the repository, so runs before and after a
    hardware change compare side by side. Build the same commit both times.

.EXAMPLE
    .\tools\buildbench\measure_build.ps1 -Label "Core Ultra 7 265K" -Runs 3

.EXAMPLE
    .\tools\buildbench\measure_build.ps1 -Restore
    Puts back the build directories an interrupted run left set aside.
#>
[CmdletBinding()]
param(
    # A name for this machine or configuration, written to the CSV.
    [string]$Label = "",
    # How many clean builds to time.
    [ValidateRange(1, 50)][int]$Runs = 1,
    # desktop: the default desktop image; console: the kernel and services only.
    [ValidateSet("desktop", "console")][string]$Image = "desktop",
    # Build the release profile of the image (run_demo.py --release).
    [switch]$Release,
    # Limit cargo's parallel jobs (CARGO_BUILD_JOBS); 0 = cargo's default.
    [ValidateRange(0, 1024)][int]$Jobs = 0,
    # One untimed clean build first: caches everything the build downloads
    # (the BIOS bootloader stages come from crates.io). Needed once per machine.
    [switch]$WarmUp,
    # Let the timed build use the network (no CARGO_NET_OFFLINE).
    [switch]$AllowNetwork,
    # Echo every build line on the console (slower; the log always has them).
    [switch]$ShowOutput,
    # Do not ask before starting.
    [switch]$Yes,
    # Only put back directories an interrupted run left set aside, then exit.
    [switch]$Restore,
    # Where results go; outside the repository so nothing here touches them.
    [string]$ResultsDir = (Join-Path $env:LOCALAPPDATA "lazyos-build-bench")
)

$ErrorActionPreference = "Stop"
# The `cmd /c "..."` lines rely on 5.1's quoting; PowerShell 7.3+ escapes otherwise.
$PSNativeCommandArgumentPassing = "Legacy"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$BuildDirs = @("target", "xui-app\target", "rhai-host\target", "vendor\bootloader\target")
$SaveRoot = Join-Path $Root "shots\.buildbench-saved"
$BusyboxDir = "target\abi\busybox"
# Variables that change what is built; cleared for the run, then restored.
$ClearedEnv = '^(LAZYOS_|LAZYRAD_|RUSTFLAGS$|RUSTDOCFLAGS$|CARGO_ENCODED_RUSTFLAGS$|CARGO_PROFILE_|' +
              'CARGO_INCREMENTAL$|CARGO_TARGET_DIR$|CARGO_BUILD_|RUSTC_WRAPPER$|RUSTC_WORKSPACE_WRAPPER$|' +
              'CARGO_NET_OFFLINE$|CARGO_TERM_)'
$Invariant = [Globalization.CultureInfo]::InvariantCulture

function Say([string]$Text, [string]$Color = "Cyan") { Write-Host $Text -ForegroundColor $Color }

function Fail([string]$Text) {
    Write-Host "error: $Text" -ForegroundColor Red
    exit 1
}

function Format-Duration([double]$Seconds) {
    $span = [TimeSpan]::FromSeconds($Seconds)
    "{0:00}:{1:00}:{2:00}.{3}" -f [int][math]::Floor($span.TotalHours), $span.Minutes, $span.Seconds,
        [int][math]::Floor($span.Milliseconds / 100)
}

function Format-Number([double]$Value) { [math]::Round($Value, 1).ToString($Invariant) }

# A native command's first output line; never throws on its stderr (PS 5.1).
function Get-NativeLine([string]$Command) {
    Push-Location $Root
    try { "$(cmd /c "$Command 2>nul" | Select-Object -First 1)".Trim() } finally { Pop-Location }
}

function Get-DefenderState {
    try {
        if ((Get-MpComputerStatus).RealTimeProtectionEnabled) { "on" } else { "off" }
    } catch { "unknown" }
}

function Get-MachineInfo {
    $cpus = @(Get-CimInstance Win32_Processor)
    $ram = (Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory
    $commit = Get-NativeLine "git rev-parse --short HEAD"
    if (Get-NativeLine "git status --porcelain --untracked-files=no") { $commit = "$commit+dirty" }
    [pscustomobject]@{
        Cpu      = $cpus[0].Name.Trim()
        Cores    = ($cpus | Measure-Object NumberOfCores -Sum).Sum
        Threads  = ($cpus | Measure-Object NumberOfLogicalProcessors -Sum).Sum
        MaxMHz   = $cpus[0].MaxClockSpeed
        RamGB    = [math]::Round($ram / 1GB, 1)
        Power    = ((powercfg /getactivescheme) -replace '^.*\((.*)\)\s*$', '$1').Trim()
        Defender = Get-DefenderState
        Rustc    = Get-NativeLine "rustc --version"
        Commit   = $commit
    }
}

function Test-ConfigWrapper {
    $home_ = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
    $files = @("$home_\config.toml", "$home_\config", "$Root\.cargo\config.toml", "$Root\.cargo\config")
    foreach ($file in $files) {
        if ((Test-Path $file) -and (Select-String -Path $file -Pattern '^\s*rustc-(workspace-)?wrapper\s*=' -Quiet)) {
            Fail "$file sets a rustc wrapper (sccache?); comment it out so every run really compiles"
        }
    }
}

function Assert-Ready {
    foreach ($tool in @("python", "cargo", "rustup", "git")) {
        if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { Fail "$tool is not on PATH" }
    }
    if (Get-Process -Name "qemu-system-*" -ErrorAction SilentlyContinue) {
        Fail "QEMU is running (it may hold target\lazyos.img open); close it first"
    }
    $busy = Get-Process -Name "cargo", "rustc", "rust-analyzer" -ErrorAction SilentlyContinue
    if ($busy) {
        $names = ($busy | Select-Object -ExpandProperty ProcessName -Unique) -join ", "
        Fail "$names running: it competes for the CPU and locks files in target\; close it first"
    }
    Test-ConfigWrapper
}

function Get-SavedPath([string]$Rel) { Join-Path $SaveRoot ($Rel -replace '\\', '__') }

function Remove-Tree([string]$Path) {
    if (-not (Test-Path $Path)) { return }
    cmd /c "rd /s /q `"$Path`"" | Out-Null
    if (Test-Path $Path) { Fail "could not delete $Path (a file is in use?)" }
}

function Save-BuildDirs {
    New-Item -ItemType Directory -Path $SaveRoot | Out-Null
    foreach ($rel in $BuildDirs) {
        $path = Join-Path $Root $rel
        if (-not (Test-Path $path)) { continue }
        Say "  setting aside $rel"
        try { Move-Item $path (Get-SavedPath $rel) }
        catch { Fail "could not move $path aside ($($_.Exception.Message)); is a file open?" }
    }
}

# Deletes what the benchmark built and moves the originals back.
function Restore-BuildDirs {
    if (-not (Test-Path $SaveRoot)) { return }
    Say "Putting the original build directories back"
    foreach ($rel in $BuildDirs) {
        $saved = Get-SavedPath $rel
        if (-not (Test-Path $saved)) { continue }
        $path = Join-Path $Root $rel
        Remove-Tree $path
        Move-Item $saved $path
    }
    Remove-Item -Force -Recurse $SaveRoot
}

function Clear-BenchDirs {
    foreach ($rel in $BuildDirs) { Remove-Tree (Join-Path $Root $rel) }
    # The cached BusyBox is a Docker build, not part of the measurement.
    $cached = Join-Path (Get-SavedPath "target") ($BusyboxDir -replace '^target\\', '')
    if (Test-Path $cached) {
        $dest = Join-Path $Root $BusyboxDir
        New-Item -ItemType Directory -Force -Path $dest | Out-Null
        Get-ChildItem $cached -File | Copy-Item -Destination $dest
    }
}

function Invoke-Untimed([string]$What, [string]$Command, [switch]$Optional) {
    Say "  $What"
    Push-Location $Root
    try { cmd /c "$Command" | Out-Null } finally { Pop-Location }
    if ($LASTEXITCODE -eq 0) { return }
    if ($Optional) { Say "  warning: $What failed (exit $LASTEXITCODE)" Yellow; return }
    Fail "$What failed (exit $LASTEXITCODE)"
}

function Initialize-Downloads {
    Say "Preparing (not timed): toolchain, crates, BusyBox"
    Invoke-Untimed "rustup: the pinned toolchain and the musl target" "rustup target add x86_64-unknown-linux-musl"
    Invoke-Untimed "cargo fetch (root)" "cargo fetch --quiet"
    Invoke-Untimed "cargo fetch (xui-app)" "cargo fetch --quiet --manifest-path xui-app\Cargo.toml"
    Invoke-Untimed "cargo fetch (rhai-host)" "cargo fetch --quiet --manifest-path rhai-host\Cargo.toml"
    Invoke-Untimed "cargo fetch (bootloader)" "cargo fetch --quiet --manifest-path vendor\bootloader\Cargo.toml" -Optional
    # Only a binary built with the current configuration is used as is; a
    # stale one would make the timed build try Docker again.
    $probe = "import sys; sys.path.insert(0, 'tools/abi'); import busybox; " +
             "busybox.ensure_busybox(); sys.exit(0 if busybox._cached() else 1)"
    Invoke-Untimed "BusyBox (cached, or built once in Docker)" "python -c `"$probe`""
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

# A line that means the image is missing a part, though the build succeeded.
function Get-Problem([string]$Line) {
    if ($Line -match '^warning: (rhai did not build|no linker for the musl target|.*rhai unavailable)') { return "no rhai" }
    if ($Line -match 'busybox: (no musl-gcc|build unavailable|could not rebuild)') { return "busybox rebuilt" }
    if ($Line -match 'offline' -and $Line -match '(error|failed|unable|not found|no matching)') { return "offline" }
    return $null
}

function Invoke-TimedBuild([string]$LogFile) {
    $demoArgs = @("--build-only", "--yes", "--reset-os")
    if ($Image -eq "desktop") { $demoArgs += "--desktop" }
    if ($Release) { $demoArgs += "--release" }
    $command = "python tools\run_demo.py $($demoArgs -join ' ') 2>&1"

    $state = @{ Phase = "setup"; XuiDone = $false; Problems = @{} }
    $marks = New-Object System.Collections.ArrayList
    [void]$marks.Add(@("setup", 0.0))
    $log = New-Object IO.StreamWriter($LogFile, $false, (New-Object Text.UTF8Encoding($false)))
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
                [void]$marks.Add(@($next, $t))
                Say ("  [{0}] {1}" -f (Format-Duration $t), $next) DarkCyan
            }
            $problem = Get-Problem $line
            if ($problem) { $state.Problems[$problem] = $true }
            $stamped = "[{0}] {1}" -f (Format-Duration $t), $line
            $log.WriteLine($stamped)
            if ($ShowOutput) { Write-Host $stamped }
        }
        $exit = $LASTEXITCODE
    } finally {
        $clock.Stop()
        $log.Dispose()
        $ErrorActionPreference = $savedEap
        Pop-Location
    }
    $total = $clock.Elapsed.TotalSeconds

    # Each phase lasts until the next one starts; a phase that comes back adds up.
    $phases = @{ setup = 0.0; rhai = 0.0; xui = 0.0; image = 0.0 }
    for ($i = 0; $i -lt $marks.Count; $i++) {
        $end = if ($i + 1 -lt $marks.Count) { $marks[$i + 1][1] } else { $total }
        $phases[$marks[$i][0]] += $end - $marks[$i][1]
    }
    [pscustomobject]@{ Exit = $exit; Total = $total; Phases = $phases; Problems = @($state.Problems.Keys) }
}

function Add-ResultRow($Info, [int]$Run, $Result, [string]$CsvFile) {
    $row = [pscustomobject][ordered]@{
        date       = (Get-Date).ToString("yyyy-MM-dd HH:mm:ss")
        label      = $Label
        cpu        = $Info.Cpu
        cores      = $Info.Cores
        threads    = $Info.Threads
        max_mhz    = $Info.MaxMHz
        ram_gb     = Format-Number $Info.RamGB
        power_plan = $Info.Power
        defender   = $Info.Defender
        commit     = $Info.Commit
        rustc      = $Info.Rustc
        image      = $Image + $(if ($Release) { "-release" } else { "" })
        jobs       = $(if ($Jobs -gt 0) { $Jobs } else { "default" })
        run        = $Run
        exit_code  = $Result.Exit
        problems   = ($Result.Problems -join ";")
        total_s    = Format-Number $Result.Total
        setup_s    = Format-Number $Result.Phases.setup
        rhai_s     = Format-Number $Result.Phases.rhai
        xui_s      = Format-Number $Result.Phases.xui
        image_s    = Format-Number $Result.Phases.image
    }
    try {
        $row | Export-Csv -Path $CsvFile -Append -NoTypeInformation -Encoding UTF8
    } catch {
        Say "warning: could not write $CsvFile (open in Excel?): $($_.Exception.Message)" Yellow
        $row | Format-List | Out-String | Write-Host
    }
}

function Test-Usable($Result) { $Result.Exit -eq 0 -and $Result.Problems.Count -eq 0 }

function Show-Summary($Results, [string]$CsvFile) {
    Say "`nResults" Green
    for ($i = 0; $i -lt $Results.Count; $i++) {
        $r = $Results[$i]; $p = $r.Phases
        $status = if ($r.Exit -ne 0) { "FAILED (exit $($r.Exit))" }
                  elseif ($r.Problems.Count) { "INCOMPLETE ($($r.Problems -join ', '))" } else { "ok" }
        "  run {0}: total {1}  (setup {2}, rhai {3}, xui apps {4}, cargo image {5})  {6}" -f ($i + 1),
            (Format-Duration $r.Total), (Format-Duration $p.setup), (Format-Duration $p.rhai),
            (Format-Duration $p.xui), (Format-Duration $p.image), $status | Write-Host
    }
    $ok = @($Results | Where-Object { Test-Usable $_ } | ForEach-Object { $_.Total } | Sort-Object)
    if ($ok.Count -gt 1) {
        $median = if ($ok.Count % 2) { $ok[[int][math]::Floor($ok.Count / 2)] }
                  else { ($ok[$ok.Count / 2 - 1] + $ok[$ok.Count / 2]) / 2 }
        $mean = ($ok | Measure-Object -Average).Average
        "  best {0}  median {1}  mean {2}" -f (Format-Duration $ok[0]), (Format-Duration $median),
            (Format-Duration $mean) | Write-Host
    }
    if (Test-Path $CsvFile) {
        Say "`nAll runs so far ($CsvFile):" Green
        Import-Csv $CsvFile | Format-Table date, label, cpu, threads, commit, image, run, exit_code, problems,
            total_s, rhai_s, xui_s, image_s -AutoSize | Out-String | Write-Host
    }
}

function Invoke-Runs($Info, [string]$CsvFile) {
    $stamp = (Get-Date).ToString("yyyyMMdd-HHmmss")
    $results = @()
    $first = if ($WarmUp) { 0 } else { 1 }
    for ($run = $first; $run -le $Runs; $run++) {
        $name = if ($run -eq 0) { "warm-up (not recorded)" } else { "run $run of $Runs" }
        Say "`n$name" Green
        Clear-BenchDirs
        $log = Join-Path $ResultsDir "build-$stamp-run$run.log"
        if ($run -eq 0) { $env:CARGO_NET_OFFLINE = $null }
        elseif (-not $AllowNetwork) { $env:CARGO_NET_OFFLINE = "true" }
        Say "  building (log: $log)"
        $result = Invoke-TimedBuild $log
        "  total {0}" -f (Format-Duration $result.Total) | Write-Host
        if ($result.Exit -ne 0) {
            Say "the build failed; see $log" Red
            if ($result.Problems -contains "offline") {
                Say "cargo needed a download: run once with -WarmUp (or -AllowNetwork)" Yellow
            }
            if ($run -gt 0) { $results += $result; Add-ResultRow $Info $run $result $CsvFile }
            break
        }
        if ($run -gt 0) { $results += $result; Add-ResultRow $Info $run $result $CsvFile }
    }
    $results
}

# --- main ------------------------------------------------------------------

if ($Restore) {
    if (-not (Test-Path $SaveRoot)) { Say "nothing to restore" Green; exit 0 }
    Restore-BuildDirs
    exit 0
}
if (Test-Path $SaveRoot) {
    Fail "an interrupted run left the build directories in $SaveRoot; run with -Restore first"
}
Assert-Ready
$info = Get-MachineInfo
Say "LazyOS clean-build benchmark" Green
"  CPU      {0} ({1} cores / {2} threads, {3} MHz)" -f $info.Cpu, $info.Cores, $info.Threads, $info.MaxMHz | Write-Host
"  RAM      {0} GB    power plan: {1}    Defender real-time: {2}" -f $info.RamGB, $info.Power, $info.Defender | Write-Host
"  commit   {0}    {1}" -f $info.Commit, $info.Rustc | Write-Host
"  image    {0}{1}, {2} run(s){3}, jobs: {4}" -f $Image, $(if ($Release) { " (release)" } else { "" }), $Runs,
    $(if ($WarmUp) { " + warm-up" } else { "" }), $(if ($Jobs -gt 0) { $Jobs } else { "default" }) | Write-Host
$cleared = @(Get-ChildItem env: | Where-Object { $_.Name -match $ClearedEnv } | Select-Object -ExpandProperty Name)
if ($cleared.Count) { "  cleared  {0}" -f ($cleared -join ", ") | Write-Host }

if (-not $Yes) {
    Write-Host "`nThe build directories are set aside (renamed, not deleted) and put back at the end;" -ForegroundColor Yellow
    Write-Host "each run needs the disk space of a full build." -ForegroundColor Yellow
    if ((Read-Host "Proceed? [y/N]") -notmatch '^(y|yes)$') { exit 1 }
}

New-Item -ItemType Directory -Force -Path $ResultsDir | Out-Null
$csv = Join-Path $ResultsDir "results.csv"

$savedEnv = @{}
foreach ($name in $cleared + @("CARGO_BUILD_JOBS", "CARGO_NET_OFFLINE", "CARGO_TERM_COLOR",
                               "CARGO_TERM_PROGRESS_WHEN", "PYTHONUNBUFFERED", "PYTHONIOENCODING")) {
    $savedEnv[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
}
$savedEncoding = [Console]::OutputEncoding
$exitCode = 0
try {
    foreach ($name in $cleared) { [Environment]::SetEnvironmentVariable($name, $null, "Process") }
    $env:CARGO_TERM_COLOR = "never"
    $env:CARGO_TERM_PROGRESS_WHEN = "never"
    $env:PYTHONUNBUFFERED = "1"
    $env:PYTHONIOENCODING = "utf-8"
    [Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
    if ($Jobs -gt 0) { $env:CARGO_BUILD_JOBS = "$Jobs" }

    Initialize-Downloads
    Save-BuildDirs
    $results = @(Invoke-Runs $info $csv)
    Show-Summary $results $csv
    if ($results.Count -lt $Runs -or @($results | Where-Object { -not (Test-Usable $_) }).Count) { $exitCode = 1 }
} finally {
    Restore-BuildDirs
    [Console]::OutputEncoding = $savedEncoding
    foreach ($name in $savedEnv.Keys) {
        [Environment]::SetEnvironmentVariable($name, $savedEnv[$name], "Process")
    }
}
exit $exitCode
