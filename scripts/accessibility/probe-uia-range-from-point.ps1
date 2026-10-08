param(
    [string]$ProcessName = "alacritty",
    [string]$TitlePattern = "*",
    [int]$TargetProcessId = 0,
    [switch]$VerifyLayoutChanges
)

$ErrorActionPreference = "Stop"

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName WindowsBase

if ($VerifyLayoutChanges) {
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class AlacrittyRangeGeometryProbeNative {
    [StructLayout(LayoutKind.Sequential)]
    public struct Rect {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);

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

function Assert-RangeRoundTrip {
    param(
        [System.Windows.Automation.TextPattern]$Pattern,
        [System.Windows.Automation.Text.TextPatternRange]$Range,
        [string]$ExpectedText
    )

    $rectangles = @($Range.GetBoundingRectangles())
    if ($rectangles.Count -lt 1) {
        Write-Error "GetBoundingRectangles returned no rectangle."
    }
    $rectangle = $rectangles[0]
    if ($rectangle.Width -le 0 -or $rectangle.Height -le 0) {
        Write-Error "GetBoundingRectangles returned a non-positive rectangle."
    }

    $point = [System.Windows.Point]::new(
        $rectangle.Left + $rectangle.Width / 2.0,
        $rectangle.Top + $rectangle.Height / 2.0
    )
    $roundTrip = $Pattern.RangeFromPoint($point)
    $roundTrip.ExpandToEnclosingUnit([System.Windows.Automation.Text.TextUnit]::Line)
    if ($roundTrip.GetText(-1) -ne $ExpectedText) {
        Write-Error "RangeFromPoint did not round-trip the bounding rectangle to its line."
    }

    return $rectangles
}

function Find-AlacrittyWindow {
    param([int]$RequiredProcessId)

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
        if ($RequiredProcessId -gt 0 -and $windowProcessId -ne $RequiredProcessId) {
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

$window = Find-AlacrittyWindow -RequiredProcessId $TargetProcessId
if ($null -eq $window) {
    Write-Error "No matching Alacritty window found for process '$ProcessName' and title '$TitlePattern'."
}

$pattern = $null
if (-not $window.TryGetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern, [ref]$pattern)) {
    Write-Error "TextPattern is not available for '$($window.Current.Name)'."
}

$rect = $window.Current.BoundingRectangle
if ($rect.IsEmpty -or $rect.Width -le 0 -or $rect.Height -le 0) {
    Write-Error "Window bounding rectangle is empty."
}

$x = $rect.Left + [Math]::Min(140.0, [Math]::Max(1.0, $rect.Width / 2.0))
$maxY = [Math]::Min($rect.Height - 1.0, 260.0)
$line = $null

for ($yOffset = 20.0; $yOffset -le $maxY; $yOffset += 15.0) {
    $point = [System.Windows.Point]::new($x, $rect.Top + $yOffset)
    $range = $pattern.RangeFromPoint($point)
    if ($null -eq $range) {
        continue
    }

    $range.ExpandToEnclosingUnit([System.Windows.Automation.Text.TextUnit]::Line)
    $candidate = $range.GetText(-1)
    if (-not [string]::IsNullOrWhiteSpace($candidate)) {
        $line = $candidate
        break
    }
}

if ([string]::IsNullOrWhiteSpace($line)) {
    Write-Error "RangeFromPoint did not return readable text for any sampled viewport row."
}

$initialRectangles = Assert-RangeRoundTrip -Pattern $pattern -Range $range -ExpectedText $line

if ($VerifyLayoutChanges) {
    $handle = [IntPtr]$window.Current.NativeWindowHandle
    $windowRect = New-Object AlacrittyRangeGeometryProbeNative+Rect
    if (-not [AlacrittyRangeGeometryProbeNative]::GetWindowRect($handle, [ref]$windowRect)) {
        Write-Error "Could not read the Alacritty window rectangle."
    }

    $width = $windowRect.Right - $windowRect.Left
    $height = $windowRect.Bottom - $windowRect.Top
    $noActivateOrZOrder = 0x0014
    if (-not [AlacrittyRangeGeometryProbeNative]::SetWindowPos(
        $handle,
        [IntPtr]::Zero,
        $windowRect.Left + 80,
        $windowRect.Top + 40,
        $width,
        $height,
        $noActivateOrZOrder
    )) {
        Write-Error "Could not move the Alacritty window."
    }
    Start-Sleep -Milliseconds 500

    $movedRectangles = Assert-RangeRoundTrip -Pattern $pattern -Range $range -ExpectedText $line
    if (
        $movedRectangles[0].Left -eq $initialRectangles[0].Left `
            -and $movedRectangles[0].Top -eq $initialRectangles[0].Top
    ) {
        Write-Error "Retained range geometry did not follow the window move."
    }

    if (-not [AlacrittyRangeGeometryProbeNative]::SetWindowPos(
        $handle,
        [IntPtr]::Zero,
        $windowRect.Left + 80,
        $windowRect.Top + 40,
        $width + 160,
        $height + 80,
        $noActivateOrZOrder
    )) {
        Write-Error "Could not resize the Alacritty window."
    }
    Start-Sleep -Milliseconds 500

    $firstLine = $pattern.DocumentRange.Clone()
    $firstLine.ExpandToEnclosingUnit([System.Windows.Automation.Text.TextUnit]::Line)
    $firstLineText = $firstLine.GetText(-1)
    Assert-RangeRoundTrip -Pattern $pattern -Range $firstLine -ExpectedText $firstLineText | Out-Null
    Write-Output "Move and resize geometry refresh: PASS"
}

Write-Output "RangeFromPoint line read: PASS"
Write-Output "Bounding rectangle round-trip: PASS"
