[CmdletBinding()]
param(
    [ValidateSet("master", "nvda", "integration")]
    [string[]]$Matrix = @("master", "nvda", "integration"),
    [ValidateRange(0.01, 1440.0)]
    [double]$DurationMinutes = 30,
    [ValidateRange(0.01, 3600.0)]
    [double]$MarkerIntervalSeconds = 0.1,
    [ValidateRange(1, 60000)]
    [int]$InternalLatencyMilliseconds = 2000,
    [ValidateRange(1, 60000)]
    [int]$ObserverLatencyMilliseconds = 2000,
    [ValidateRange(1, 1000000)]
    [int]$TraceCapacity = 200000,
    [string]$ArtifactRoot = "",
    [string]$BuildRoot = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Invoke-NativeCommand {
    param(
        [Parameter(Mandatory)]
        [string]$FilePath,
        [Parameter(Mandatory)]
        [string[]]$ArgumentList,
        [Parameter(Mandatory)]
        [string]$WorkingDirectory,
        [Parameter(Mandatory)]
        [string]$LogPath,
        [Parameter(Mandatory)]
        [TimeSpan]$Timeout
    )

    $stdoutPath = "$LogPath.stdout.log"
    $stderrPath = "$LogPath.stderr.log"
    $process = Start-Process -FilePath $FilePath -ArgumentList $ArgumentList `
        -WorkingDirectory $WorkingDirectory -PassThru -NoNewWindow `
        -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath
    if (-not $process.WaitForExit([int][Math]::Min($Timeout.TotalMilliseconds, [int]::MaxValue))) {
        throw "Timed out waiting for PID $($process.Id): $FilePath $($ArgumentList -join ' '). The process was not stopped."
    }
    if ($process.ExitCode -ne 0) {
        throw "Command exited with code $($process.ExitCode): $FilePath $($ArgumentList -join ' '). See '$stdoutPath' and '$stderrPath'."
    }
}

function Add-T013NativeWindowApi {
    if ("T013.NativeWindow" -as [type]) {
        return
    }

    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

namespace T013 {
    public sealed class WindowInfo {
        public IntPtr Handle { get; set; }
        public uint ProcessId { get; set; }
        public string Title { get; set; }
    }

    public static class NativeWindow {
        private delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr lParam);

        [DllImport("user32.dll")]
        private static extern bool EnumWindows(EnumWindowsProc callback, IntPtr lParam);

        [DllImport("user32.dll")]
        private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);

        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern int GetWindowTextW(IntPtr hwnd, StringBuilder text, int count);

        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool IsWindow(IntPtr hwnd);

        [DllImport("user32.dll")]
        private static extern bool IsWindowVisible(IntPtr hwnd);

        [DllImport("user32.dll")]
        private static extern IntPtr GetWindow(IntPtr hwnd, uint command);

        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        private static extern int GetClassNameW(IntPtr hwnd, StringBuilder className, int count);

        [DllImport("user32.dll")]
        private static extern IntPtr GetForegroundWindow();

        [DllImport("user32.dll")]
        private static extern bool SetForegroundWindow(IntPtr hwnd);

        [DllImport("kernel32.dll")]
        private static extern bool QueryPerformanceFrequency(out long frequency);

        public static IntPtr Foreground() { return GetForegroundWindow(); }
        public static bool Activate(IntPtr hwnd) { return SetForegroundWindow(hwnd); }
        public static long PerformanceFrequency() {
            long frequency;
            return QueryPerformanceFrequency(out frequency) ? frequency : 0;
        }

        public static WindowInfo[] ForProcess(uint expectedProcessId) {
            var result = new List<WindowInfo>();
            EnumWindows((hwnd, ignored) => {
                uint processId;
                GetWindowThreadProcessId(hwnd, out processId);
                var className = new StringBuilder(256);
                GetClassNameW(hwnd, className, className.Capacity);
                if (processId == expectedProcessId && IsWindowVisible(hwnd) && GetWindow(hwnd, 4) == IntPtr.Zero
                    && className.ToString() == "Window Class") {
                    var text = new StringBuilder(32768);
                    GetWindowTextW(hwnd, text, text.Capacity);
                    result.Add(new WindowInfo { Handle = hwnd, ProcessId = processId, Title = text.ToString() });
                }
                return true;
            }, IntPtr.Zero);
            return result.ToArray();
        }

        public static string Title(IntPtr hwnd) {
            if (!IsWindow(hwnd)) return null;
            var text = new StringBuilder(32768);
            GetWindowTextW(hwnd, text, text.Capacity);
            return text.ToString();
        }
    }
}
'@
}

function Write-JsonLine {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [object]$Value
    )

    $line = $Value | ConvertTo-Json -Compress -Depth 8
    [System.IO.File]::AppendAllText($Path, "$line`r`n", [System.Text.UTF8Encoding]::new($false))
}

function Write-T013ChildScript {
    param([Parameter(Mandatory)][string]$Path)

    $source = @'
param(
    [Parameter(Mandatory)][string]$RunId,
    [Parameter(Mandatory)][string]$ProducerTrace,
    [Parameter(Mandatory)][int]$DurationMilliseconds,
    [Parameter(Mandatory)][int]$IntervalMilliseconds,
    [Parameter(Mandatory)][int]$StartupDelayMilliseconds
)
$ErrorActionPreference = "Stop"
$output = [Console]::OpenStandardOutput()
$encoding = [System.Text.UTF8Encoding]::new($false)
$clock = [System.Diagnostics.Stopwatch]::StartNew()
Start-Sleep -Milliseconds $StartupDelayMilliseconds
$deadline = [System.Diagnostics.Stopwatch]::GetTimestamp() +
    [long]($DurationMilliseconds * [System.Diagnostics.Stopwatch]::Frequency / 1000)
$sequence = 0L
while ([System.Diagnostics.Stopwatch]::GetTimestamp() -lt $deadline) {
    $sequence++
    $ticks = [System.Diagnostics.Stopwatch]::GetTimestamp()
    $record = @{ run = $RunId; seq = $sequence; stage = "producer"; ticks = $ticks; bytes = 0 }
    $line = ($record | ConvertTo-Json -Compress) + "`r`n"
    [System.IO.File]::AppendAllText($ProducerTrace, $line, $encoding)
    $marker = "T013:$RunId`:$sequence`:$ticks"
    $bytes = $encoding.GetBytes("$marker`r`n`e]0;$marker`a")
    $output.Write($bytes, 0, $bytes.Length)
    $output.Flush()
    $remaining = $deadline - [System.Diagnostics.Stopwatch]::GetTimestamp()
    if ($remaining -gt 0) {
        $remainingMilliseconds = [int][Math]::Ceiling($remaining * 1000 / [System.Diagnostics.Stopwatch]::Frequency)
        Start-Sleep -Milliseconds ([Math]::Min($IntervalMilliseconds, $remainingMilliseconds))
    }
}
$output.Flush()
$clock.Stop()
'@
    [System.IO.File]::WriteAllText($Path, $source, [System.Text.UTF8Encoding]::new($false))
}

function Get-VariantDefinition {
    param([Parameter(Mandatory)][string]$Name)

    switch ($Name) {
        "master" { return @{ Name = $Name; Ref = "master" } }
        "nvda" { return @{ Name = $Name; Ref = "nvda-accessibility-layer" } }
        "integration" { return @{ Name = $Name; Ref = "windows-nvda-accessibility" } }
    }
}

function Copy-DiagnosticOverlay {
    param(
        [Parameter(Mandatory)][string]$SourceRepository,
        [Parameter(Mandatory)][string]$DestinationRepository,
        [Parameter(Mandatory)][string]$VariantDirectory
    )

    $tracked = @(& git -C $SourceRepository diff --name-only --diff-filter=ACMRT HEAD -- "*.rs" "Cargo.toml" "Cargo.lock")
    if ($LASTEXITCODE -ne 0) {
        throw "Could not enumerate tracked diagnostic changes."
    }
    $untracked = @(& git -C $SourceRepository ls-files --others --exclude-standard -- "*.rs" "Cargo.toml" "Cargo.lock")
    if ($LASTEXITCODE -ne 0) {
        throw "Could not enumerate untracked diagnostic changes."
    }

    $patchPath = Join-Path $VariantDirectory "diagnostic-overlay.patch"
    & git -C $SourceRepository diff --binary HEAD -- "*.rs" "Cargo.toml" "Cargo.lock" > $patchPath
    if ($LASTEXITCODE -ne 0) {
        throw "Could not create the diagnostic overlay patch."
    }
    if ((Get-Item -LiteralPath $patchPath).Length -gt 0) {
        Invoke-NativeCommand -FilePath "git" -ArgumentList @("apply", "--3way", $patchPath) `
            -WorkingDirectory $DestinationRepository -LogPath (Join-Path $VariantDirectory "overlay-apply") `
            -Timeout ([TimeSpan]::FromMinutes(2))
    }

    foreach ($relativePath in $untracked) {
        $destination = Join-Path $DestinationRepository $relativePath
        $destinationParent = Split-Path -Parent $destination
        New-Item -ItemType Directory -Path $destinationParent -Force | Out-Null
        Copy-Item -LiteralPath (Join-Path $SourceRepository $relativePath) -Destination $destination
    }

    return @($tracked + $untracked | Sort-Object -Unique)
}

function Wait-T013Window {
    param(
        [Parameter(Mandatory)][int]$ProcessId,
        [Parameter(Mandatory)][TimeSpan]$Timeout
    )

    $deadline = [DateTime]::UtcNow + $Timeout
    do {
        $process = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
        if ($null -eq $process) {
            throw "PID $ProcessId exited before a top-level HWND was observed."
        }
        $windows = @([T013.NativeWindow]::ForProcess([uint32]$ProcessId))
        if ($windows.Count -eq 1) {
            return $windows[0]
        }
        if ($windows.Count -gt 1) {
            $titled = @($windows | Where-Object { -not [string]::IsNullOrEmpty($_.Title) })
            if ($titled.Count -eq 1) {
                return $titled[0]
            }
        }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $deadline)

    throw "No unique top-level HWND was found for exact PID $ProcessId within $($Timeout.TotalSeconds) seconds."
}

function Get-T013TextPattern {
    param([Parameter(Mandatory)][IntPtr]$WindowHandle)

    try {
        $element = [System.Windows.Automation.AutomationElement]::FromHandle($WindowHandle)
        if ($null -eq $element) {
            return $null
        }
        $available = [bool]$element.GetCurrentPropertyValue(
            [System.Windows.Automation.AutomationElement]::IsTextPatternAvailableProperty
        )
        if (-not $available) {
            $condition = [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::IsTextPatternAvailableProperty,
                $true
            )
            $element = $element.FindFirst([System.Windows.Automation.TreeScope]::Subtree, $condition)
        }
        if ($null -eq $element) {
            return $null
        }
        return [System.Windows.Automation.TextPattern]$element.GetCurrentPattern(
            [System.Windows.Automation.TextPattern]::Pattern
        )
    } catch {
        return $null
    }
}

function Watch-T013Run {
    param(
        [Parameter(Mandatory)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory)][IntPtr]$WindowHandle,
        [Parameter(Mandatory)][string]$RunId,
        [Parameter(Mandatory)][string]$ObserverTrace,
        [Parameter(Mandatory)][string]$SampleLog,
        [Parameter(Mandatory)][bool]$RequireUia,
        [Parameter(Mandatory)][TimeSpan]$Timeout
    )

    $seen = [System.Collections.Generic.HashSet[long]]::new()
    $uiaSeen = [System.Collections.Generic.HashSet[long]]::new()
    $textPattern = $null
    $lastTitle = $null
    $measurementStarted = $false
    $deadline = [DateTime]::UtcNow + $Timeout
    while (-not $Process.HasExited -and [DateTime]::UtcNow -lt $deadline) {
        if ($measurementStarted -and [T013.NativeWindow]::Foreground() -eq $WindowHandle) {
            throw "Dedicated Alacritty PID $($Process.Id) regained focus during the background run."
        }
        $title = [T013.NativeWindow]::Title($WindowHandle)
        if ($null -eq $title) {
            $Process.Refresh()
            if ($Process.HasExited) {
                break
            }
            throw "HWND 0x$($WindowHandle.ToInt64().ToString('X')) for exact PID $($Process.Id) disappeared before process exit."
        }
        $match = [regex]::Match($title, "^T013:$([regex]::Escape($RunId)):(\d+):(\d+)$")
        $measurementStarted = $measurementStarted -or $match.Success
        if ($title -ne $lastTitle) {
            Write-JsonLine -Path $SampleLog -Value ([ordered]@{
                    ticks = [System.Diagnostics.Stopwatch]::GetTimestamp()
                    title = $title
                })
            $lastTitle = $title
        }
        if ($match.Success) {
            $sequence = [long]$match.Groups[1].Value
            if ($seen.Add($sequence)) {
                Write-JsonLine -Path $ObserverTrace -Value ([ordered]@{
                        run = $RunId
                        seq = $sequence
                        stage = "native_title"
                        ticks = [System.Diagnostics.Stopwatch]::GetTimestamp()
                        bytes = $title.Length
                        pid = $Process.Id
                        hwnd = "0x$($WindowHandle.ToInt64().ToString('X'))"
                    })
            }
            if ($RequireUia) {
                if ($null -eq $textPattern) {
                    $textPattern = Get-T013TextPattern -WindowHandle $WindowHandle
                }
                if ($null -eq $textPattern) {
                    Start-Sleep -Milliseconds 20
                    continue
                }
                try {
                    $content = $textPattern.DocumentRange.GetText(-1)
                } catch {
                    $textPattern = $null
                    Start-Sleep -Milliseconds 20
                    continue
                }
                $contentMatches = [regex]::Matches(
                    $content,
                    "T013:$([regex]::Escape($RunId)):(\d+):(\d+)"
                )
                if ($contentMatches.Count -gt 0) {
                    $uiaSequence = [long]$contentMatches[$contentMatches.Count - 1].Groups[1].Value
                    if ($uiaSeen.Add($uiaSequence)) {
                        Write-JsonLine -Path $ObserverTrace -Value ([ordered]@{
                                run = $RunId
                                seq = $uiaSequence
                                stage = "uia_content"
                                ticks = [System.Diagnostics.Stopwatch]::GetTimestamp()
                                bytes = $content.Length
                                pid = $Process.Id
                                hwnd = "0x$($WindowHandle.ToInt64().ToString('X'))"
                            })
                    }
                }
            }
        }
        Start-Sleep -Milliseconds 20
    }

    if (-not $Process.HasExited) {
        throw "Timed out waiting for Alacritty PID $($Process.Id). It was not stopped; HWND is 0x$($WindowHandle.ToInt64().ToString('X'))."
    }
    $Process.WaitForExit()
    return [pscustomobject]@{ NativeTitles = $seen.Count; UiaContents = $uiaSeen.Count }
}

$repository = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$assertScript = Join-Path $PSScriptRoot "assert-background-event-trace.ps1"
if (-not (Test-Path -LiteralPath $assertScript -PathType Leaf)) {
    throw "Trace assertion script not found: '$assertScript'."
}
. $assertScript

if ([string]::IsNullOrWhiteSpace($ArtifactRoot)) {
    $ArtifactRoot = Join-Path $repository ("artifacts\t013-{0:yyyyMMdd-HHmmss}" -f [DateTime]::Now)
}
$ArtifactRoot = [System.IO.Path]::GetFullPath($ArtifactRoot)
if (-not [string]::IsNullOrWhiteSpace($BuildRoot)) {
    $BuildRoot = [System.IO.Path]::GetFullPath($BuildRoot)
}
New-Item -ItemType Directory -Path $ArtifactRoot -Force | Out-Null

Add-T013NativeWindowApi
if ($Matrix -contains "nvda" -or $Matrix -contains "integration") {
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
}
$performanceFrequency = [T013.NativeWindow]::PerformanceFrequency()
$childScript = Join-Path $ArtifactRoot "t013-child.ps1"
Write-T013ChildScript -Path $childScript
$durationMilliseconds = [Math]::Max(1, [int][Math]::Ceiling($DurationMinutes * 60000))
$intervalMilliseconds = [Math]::Max(1, [int][Math]::Round($MarkerIntervalSeconds * 1000))
$runTimeout = [TimeSpan]::FromMilliseconds($durationMilliseconds + 30000)
$buildTimeout = [TimeSpan]::FromMinutes(45)
$summary = [System.Collections.Generic.List[object]]::new()

foreach ($matrixName in $Matrix) {
    $variant = Get-VariantDefinition -Name $matrixName
    $variantDirectory = Join-Path $ArtifactRoot $variant.Name
    if ([string]::IsNullOrWhiteSpace($BuildRoot)) {
        $worktree = Join-Path $variantDirectory "worktree"
        $targetDirectory = Join-Path $variantDirectory "target"
    } else {
        $worktree = Join-Path $BuildRoot "$($variant.Name)\worktree"
        $targetDirectory = Join-Path $BuildRoot "$($variant.Name)\target"
    }
    $traceDirectory = Join-Path $variantDirectory "trace"
    New-Item -ItemType Directory -Path $variantDirectory, $traceDirectory, $targetDirectory -Force | Out-Null

    $entry = [ordered]@{
        variant = $variant.Name
        ref = $variant.Ref
        status = "failed"
        startedUtc = [DateTime]::UtcNow.ToString("o")
        durationMinutes = $DurationMinutes
        artifactDirectory = $variantDirectory
        worktree = $worktree
        targetDirectory = $targetDirectory
        commit = $null
        overlayFiles = @()
        overlayHash = $null
        executableHash = $null
        sourceStatus = @()
        sourceFileHashes = $null
        runId = $null
        pid = $null
        hwnd = $null
        observedTitles = 0
        observedUiaContents = 0
        unfocused = $false
        assertion = $null
        error = $null
        errorStack = $null
    }

    try {
        if ([string]::IsNullOrWhiteSpace($BuildRoot)) {
            Invoke-NativeCommand -FilePath "git" -ArgumentList @("worktree", "add", "--detach", $worktree, $variant.Ref) `
                -WorkingDirectory $repository -LogPath (Join-Path $variantDirectory "worktree-add") `
                -Timeout ([TimeSpan]::FromMinutes(2))
            $entry.overlayFiles = @(Copy-DiagnosticOverlay -SourceRepository $repository `
                    -DestinationRepository $worktree -VariantDirectory $variantDirectory)
        } elseif (-not (Test-Path -LiteralPath $worktree -PathType Container)) {
            throw "Reusable worktree not found: '$worktree'."
        } else {
            $buildManifestPath = Join-Path $BuildRoot "$($variant.Name)\build-manifest.json"
            if (-not (Test-Path -LiteralPath $buildManifestPath -PathType Leaf)) {
                throw "Reusable build manifest not found: '$buildManifestPath'."
            }
            $buildManifest = Get-Content -LiteralPath $buildManifestPath | ConvertFrom-Json
            $entry.overlayFiles = @($buildManifest.overlayFiles)
            $entry.overlayHash = $buildManifest.overlayHash
            $entry.sourceStatus = @($buildManifest.sourceStatus)
            $entry.sourceFileHashes = $buildManifest.sourceFileHashes
        }
        $entry.commit = (& git -C $worktree rev-parse HEAD).Trim()
        if ($LASTEXITCODE -ne 0) {
            throw "Could not resolve the variant commit."
        }

        if ([string]::IsNullOrWhiteSpace($BuildRoot)) {
            $oldTargetDirectory = $env:CARGO_TARGET_DIR
            try {
                $env:CARGO_TARGET_DIR = $targetDirectory
                Invoke-NativeCommand -FilePath "cargo" `
                    -ArgumentList @("build", "-p", "alacritty", "--target", "x86_64-pc-windows-msvc") `
                    -WorkingDirectory $worktree -LogPath (Join-Path $variantDirectory "cargo-build") -Timeout $buildTimeout
            } finally {
                $env:CARGO_TARGET_DIR = $oldTargetDirectory
            }
        }

        $executable = Join-Path $targetDirectory "x86_64-pc-windows-msvc\debug\alacritty.exe"
        if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
            throw "Built executable not found: '$executable'."
        }
        $entry.executableHash = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash
        $sourceFileHashes = [ordered]@{}
        foreach ($relativePath in $entry.overlayFiles) {
            $sourceFileHashes[$relativePath] = (
                Get-FileHash -LiteralPath (Join-Path $worktree $relativePath) -Algorithm SHA256
            ).Hash
        }
        $entry.sourceFileHashes = $sourceFileHashes
        if ([string]::IsNullOrWhiteSpace($BuildRoot)) {
            $overlayPatchPath = Join-Path $variantDirectory "diagnostic-overlay.patch"
            $entry.overlayHash = (Get-FileHash -LiteralPath $overlayPatchPath -Algorithm SHA256).Hash
            $entry.sourceStatus = @(& git -C $worktree status --porcelain)
            [ordered]@{
                commit = $entry.commit
                overlayFiles = $entry.overlayFiles
                overlayHash = $entry.overlayHash
                executableHash = $entry.executableHash
                sourceStatus = $entry.sourceStatus
                sourceFileHashes = $entry.sourceFileHashes
            } | ConvertTo-Json -Depth 8 |
                Set-Content -LiteralPath (Join-Path $variantDirectory "build-manifest.json") -Encoding utf8
        } elseif ($entry.commit -ne $buildManifest.commit -or
            $entry.executableHash -ne $buildManifest.executableHash -or
            ($entry.sourceFileHashes | ConvertTo-Json -Compress) -ne
                ($buildManifest.sourceFileHashes | ConvertTo-Json -Compress)) {
            throw "Reusable build provenance does not match its manifest for $($variant.Name)."
        }

        $runId = "$($variant.Name)-$([Guid]::NewGuid().ToString('N'))"
        $entry.runId = $runId
        $rustTrace = Join-Path $traceDirectory "rust.jsonl"
        $producerTrace = Join-Path $traceDirectory "producer.jsonl"
        $observerTrace = Join-Path $traceDirectory "observer.jsonl"
        $observerSamples = Join-Path $traceDirectory "observer-samples.log"
        $configPath = Join-Path $variantDirectory "alacritty.toml"
        Set-Content -LiteralPath $configPath -Value "" -Encoding utf8
        $pwshPath = (Get-Process -Id $PID).Path
        $arguments = @(
            "--config-file", $configPath,
            "--option", "window.dynamic_title=true",
            "--command", $pwshPath, "-NoLogo", "-NoProfile", "-NonInteractive",
            "-File", $childScript, "-RunId", $runId, "-ProducerTrace", $producerTrace,
            "-DurationMilliseconds", $durationMilliseconds, "-IntervalMilliseconds", $intervalMilliseconds,
            "-StartupDelayMilliseconds", 10000
        )

        $oldTrace = $env:ALACRITTY_T013_TRACE
        $oldRunId = $env:ALACRITTY_T013_RUN_ID
        $oldCapacity = $env:ALACRITTY_T013_TRACE_CAPACITY
        try {
            $foregroundBefore = [T013.NativeWindow]::Foreground()
            $env:ALACRITTY_T013_TRACE = $rustTrace
            $env:ALACRITTY_T013_RUN_ID = $runId
            $env:ALACRITTY_T013_TRACE_CAPACITY = [string]$TraceCapacity
            $process = Start-Process -FilePath $executable -ArgumentList $arguments -WorkingDirectory $worktree `
                -PassThru
        } finally {
            $env:ALACRITTY_T013_TRACE = $oldTrace
            $env:ALACRITTY_T013_RUN_ID = $oldRunId
            $env:ALACRITTY_T013_TRACE_CAPACITY = $oldCapacity
        }

        $entry.pid = $process.Id
        $window = Wait-T013Window -ProcessId $process.Id -Timeout ([TimeSpan]::FromSeconds(15))
        $entry.hwnd = "0x$($window.Handle.ToInt64().ToString('X'))"
        if ([T013.NativeWindow]::Foreground() -eq $window.Handle -and
            $foregroundBefore -ne [IntPtr]::Zero -and $foregroundBefore -ne $window.Handle) {
            [void][T013.NativeWindow]::Activate($foregroundBefore)
        }
        $focusDeadline = [DateTime]::UtcNow.AddSeconds(5)
        while ([T013.NativeWindow]::Foreground() -eq $window.Handle -and
            [DateTime]::UtcNow -lt $focusDeadline) {
            Start-Sleep -Milliseconds 50
        }
        $entry.unfocused = [T013.NativeWindow]::Foreground() -ne $window.Handle
        if (-not $entry.unfocused) {
            throw "Dedicated Alacritty PID $($process.Id) could not be kept unfocused."
        }
        $observation = Watch-T013Run -Process $process -WindowHandle $window.Handle -RunId $runId `
            -ObserverTrace $observerTrace -SampleLog $observerSamples `
            -RequireUia ($variant.Name -ne "master") -Timeout $runTimeout
        $entry.observedTitles = $observation.NativeTitles
        $entry.observedUiaContents = $observation.UiaContents
        if ($process.ExitCode -ne 0) {
            throw "Alacritty PID $($process.Id) exited with code $($process.ExitCode)."
        }

        $entry.assertion = Test-BackgroundEventTrace -TraceDirectory $traceDirectory `
            -MaxInternalTicks ([long]($performanceFrequency * $InternalLatencyMilliseconds / 1000)) `
            -MaxObserverTicks ([long]($performanceFrequency * $ObserverLatencyMilliseconds / 1000)) `
            -RequireUia:($variant.Name -ne "master")
        if ($entry.assertion.Status -ne "converged") {
            throw "Trace assertion reported $($entry.assertion.Status) at stage $($entry.assertion.FirstStalledStage)."
        }
        $entry.status = "passed"
    } catch {
        $entry.error = $_.Exception.Message
        $entry.errorStack = $_.ScriptStackTrace
    } finally {
        $entry.finishedUtc = [DateTime]::UtcNow.ToString("o")
        $environment = [ordered]@{
            variant = $variant.Name
            ref = $variant.Ref
            commit = $entry.commit
            runId = $entry.runId
            pid = $entry.pid
            hwnd = $entry.hwnd
            durationMinutes = $DurationMinutes
            markerIntervalSeconds = $MarkerIntervalSeconds
            internalLatencyMilliseconds = $InternalLatencyMilliseconds
            observerLatencyMilliseconds = $ObserverLatencyMilliseconds
            traceCapacity = $TraceCapacity
            cargoTargetDirectory = $targetDirectory
            buildRoot = $BuildRoot
            os = [System.Environment]::OSVersion.VersionString
            powershell = $PSVersionTable.PSVersion.ToString()
            processorCount = [System.Environment]::ProcessorCount
            workingSetBytes = [System.Environment]::WorkingSet
            performanceFrequency = $performanceFrequency
            unfocused = $entry.unfocused
            capturedUtc = [DateTime]::UtcNow.ToString("o")
        }
        $environment | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $variantDirectory "environment.json") -Encoding utf8
        $entryObject = [pscustomobject]$entry
        $entryObject | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $variantDirectory "summary.json") -Encoding utf8
        $summary.Add($entryObject)
    }
}

$overall = [ordered]@{
    status = if ($summary.Status -contains "failed") { "failed" } else { "passed" }
    matrix = @($Matrix)
    durationMinutes = $DurationMinutes
    artifactRoot = $ArtifactRoot
    variants = @($summary)
}
$overall | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $ArtifactRoot "summary.json") -Encoding utf8
$overall | ConvertTo-Json -Depth 10
if ($overall.status -ne "passed") {
    exit 2
}
