param(
    [string]$ProcessName = "alacritty",
    [string]$TitlePattern = "*",
    [switch]$VerifyBackgroundRefresh,
    [int]$PhaseSeconds = 3,
    [string]$StimulusDirectory = "",
    [string]$ExpectedTitle = "",
    [string]$TracePath = ""
)

$ErrorActionPreference = "Stop"

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName WindowsBase

if ($VerifyBackgroundRefresh) {
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class AlacrittyTextPatternProbeNative {
    [StructLayout(LayoutKind.Sequential)]
    public struct Rect {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct Point {
        public int X;
        public int Y;
    }

    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hwnd);

    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);

    [DllImport("user32.dll")]
    public static extern bool GetClientRect(IntPtr hwnd, out Rect rect);

    [DllImport("user32.dll")]
    public static extern bool ClientToScreen(IntPtr hwnd, ref Point point);

    [DllImport("user32.dll")]
    public static extern bool SetWindowPos(
        IntPtr hwnd,
        IntPtr insertAfter,
        int x,
        int y,
        int width,
        int height,
        uint flags
    );
}
"@
}

function Find-AlacrittyWindow {
    $root = [System.Windows.Automation.AutomationElement]::RootElement
    $windows = $root.FindAll(
        [System.Windows.Automation.TreeScope]::Children,
        [System.Windows.Automation.Condition]::TrueCondition
    )

    foreach ($window in $windows) {
        $windowProcessId = $window.Current.ProcessId
        if ($windowProcessId -le 0) {
            continue
        }

        $process = Get-Process -Id $windowProcessId -ErrorAction SilentlyContinue
        if ($null -eq $process -or $process.ProcessName -ne $ProcessName) {
            continue
        }

        if ($window.Current.Name -like $TitlePattern) {
            return $window
        }
    }

    return $null
}

$window = Find-AlacrittyWindow
if ($null -eq $window) {
    Write-Error "No matching Alacritty window found for process '$ProcessName' and title '$TitlePattern'."
}

$pattern = $null
if (-not $window.TryGetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern, [ref]$pattern)) {
    Write-Error "TextPattern is not available for '$($window.Current.Name)'."
}

$documentText = $pattern.DocumentRange.GetText(-1)
if ([string]::IsNullOrEmpty($documentText)) {
    Write-Error "DocumentRange.GetText returned no text."
}

$visibleRanges = $pattern.GetVisibleRanges()
if ($null -eq $visibleRanges -or $visibleRanges.Count -lt 1) {
    Write-Error "GetVisibleRanges returned no visible ranges."
}

$selection = $pattern.GetSelection()
if ($null -eq $selection) {
    Write-Error "GetSelection returned null."
}

if ($VerifyBackgroundRefresh) {
    $targetHandle = [IntPtr]$window.Current.NativeWindowHandle
    $windowRect = New-Object AlacrittyTextPatternProbeNative+Rect
    if (-not [AlacrittyTextPatternProbeNative]::GetWindowRect($targetHandle, [ref]$windowRect)) {
        Write-Error "Could not read the target Alacritty window rectangle."
    }
    $initialClientOrigin = New-Object AlacrittyTextPatternProbeNative+Point
    $clientRect = New-Object AlacrittyTextPatternProbeNative+Rect
    if (-not [AlacrittyTextPatternProbeNative]::GetClientRect($targetHandle, [ref]$clientRect)) {
        Write-Error "Could not read the target Alacritty client rectangle."
    }
    if (-not [AlacrittyTextPatternProbeNative]::ClientToScreen(
        $targetHandle,
        [ref]$initialClientOrigin
    )) {
        Write-Error "Could not read the target Alacritty client origin."
    }
    $initialPointText = ""
    $pointOffsetX = [Math]::Min(140.0, [Math]::Max(1.0, $clientRect.Right / 2.0))
    $pointOffsetY = 0.0
    $maxY = [Math]::Min($clientRect.Bottom - 1.0, 260.0)
    for ($y = 20.0; $y -le $maxY; $y += 15.0) {
        $range = $pattern.RangeFromPoint(
            [System.Windows.Point]::new(
                $initialClientOrigin.X + $pointOffsetX,
                $initialClientOrigin.Y + $y
            )
        )
        if ($null -eq $range) {
            continue
        }
        $range.ExpandToEnclosingUnit([System.Windows.Automation.Text.TextUnit]::Line)
        $pointText = $range.GetText(-1)
        if (-not [string]::IsNullOrWhiteSpace($pointText)) {
            $initialPointText = $pointText
            $pointOffsetY = $y
            break
        }
    }
    if ([string]::IsNullOrEmpty($initialPointText)) {
        Write-Error "RangeFromPoint returned no baseline text in the client area."
    }

    $traceLinesBefore = @()
    if (-not [string]::IsNullOrEmpty($TracePath) -and (Test-Path -LiteralPath $TracePath)) {
        $traceLinesBefore = @(Get-Content -LiteralPath $TracePath)
    }

    $alternateHandle = [AlacrittyTextPatternProbeNative]::GetForegroundWindow()
    if ($alternateHandle -eq [IntPtr]::Zero -or $alternateHandle -eq $targetHandle) {
        $alternate = Get-Process | Where-Object {
            $_.MainWindowHandle -ne 0 -and $_.MainWindowHandle -ne $targetHandle
        } | Select-Object -First 1
        if ($null -eq $alternate -or $alternate.MainWindowHandle -eq 0) {
            Write-Error "No alternate top-level window is available for the background phase."
        }
        $alternateHandle = $alternate.MainWindowHandle
    }

    if (-not [AlacrittyTextPatternProbeNative]::SetForegroundWindow($alternateHandle)) {
        Write-Error "Could not move focus away from the target Alacritty window."
    }
    Start-Sleep -Milliseconds 250
    if ([AlacrittyTextPatternProbeNative]::GetForegroundWindow() -eq $targetHandle) {
        Write-Error "Target Alacritty window remained foreground during the background phase."
    }

    $windowWidth = $windowRect.Right - $windowRect.Left
    $windowHeight = $windowRect.Bottom - $windowRect.Top
    $noActivateOrZOrder = 0x0014
    if (-not [AlacrittyTextPatternProbeNative]::SetWindowPos(
        $targetHandle,
        [IntPtr]::Zero,
        $windowRect.Left + 80,
        $windowRect.Top + 40,
        $windowWidth + 160,
        $windowHeight + 80,
        $noActivateOrZOrder
    )) {
        Write-Error "Could not move and resize the target Alacritty window."
    }
    Start-Sleep -Milliseconds 500
    $movedClientOrigin = New-Object AlacrittyTextPatternProbeNative+Point
    if (-not [AlacrittyTextPatternProbeNative]::ClientToScreen(
        $targetHandle,
        [ref]$movedClientOrigin
    )) {
        Write-Error "Could not read the moved Alacritty client origin."
    }
    $movedRange = $pattern.RangeFromPoint(
        [System.Windows.Point]::new(
            $movedClientOrigin.X + $pointOffsetX,
            $movedClientOrigin.Y + $pointOffsetY
        )
    )
    $movedRange.ExpandToEnclosingUnit([System.Windows.Automation.Text.TextUnit]::Line)
    $movedPointText = $movedRange.GetText(-1)
    if ($movedPointText -ne $initialPointText) {
        Write-Error "RangeFromPoint did not follow the unfocused window move."
    }
    $resizedText = $pattern.DocumentRange.GetText(-1)
    if ($resizedText -eq $documentText) {
        Write-Error "UIA document shape did not follow the unfocused window resize."
    }
    if (-not [string]::IsNullOrEmpty($StimulusDirectory)) {
        New-Item -ItemType Directory -Path $StimulusDirectory -Force | Out-Null
        Set-Content -LiteralPath (Join-Path $StimulusDirectory "background.signal") `
            -Value "background"
    }
    Start-Sleep -Seconds $PhaseSeconds

    $backgroundText = $pattern.DocumentRange.GetText(-1)
    if ($backgroundText -eq $documentText) {
        Write-Error "UIA document text did not refresh while the terminal was unfocused."
    }
    if (
        -not [string]::IsNullOrEmpty($ExpectedTitle) `
            -and $window.Current.Name -ne $ExpectedTitle
    ) {
        Write-Error "UIA Name '$($window.Current.Name)' did not match '$ExpectedTitle'."
    }
    if (-not [string]::IsNullOrEmpty($TracePath)) {
        $traceLinesAfter = if (Test-Path -LiteralPath $TracePath) {
            @(Get-Content -LiteralPath $TracePath)
        } else {
            @()
        }
        $newTraceLines = @($traceLinesAfter | Select-Object -Skip $traceLinesBefore.Count)
        if (@($newTraceLines | Where-Object { $_ -eq "20015" }).Count -le 0) {
            Write-Error "No structural TextChanged event 20015 was observed."
        }
        if (@($newTraceLines | Where-Object { $_ -eq "20036" }).Count -lt 2) {
            Write-Error "Focus loss and output did not both produce active text-position event 20036."
        }
        if (@($newTraceLines | Where-Object { $_ -eq "20035" }).Count -ne 0) {
            Write-Error "Unfocused background output produced notification event 20035."
        }
        if (@($newTraceLines | Where-Object { $_ -eq "30005" }).Count -le 0) {
            Write-Error "No successful UIA Name property-change raise was observed."
        }
    }
    if ([AlacrittyTextPatternProbeNative]::GetForegroundWindow() -eq $targetHandle) {
        Write-Error "Target Alacritty window became foreground during the background phase."
    }

    Write-Output "Background TextPattern refresh: PASS"
    Write-Output "Background Name refresh: PASS"
    return
}

Write-Output "TextPattern.Available: PASS"
Write-Output "DocumentRange.GetText: PASS"
Write-Output "GetVisibleRanges: PASS"
Write-Output "GetSelection: PASS"
