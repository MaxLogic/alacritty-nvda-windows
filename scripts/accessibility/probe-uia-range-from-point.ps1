param(
    [string]$ProcessName = "alacritty",
    [string]$TitlePattern = "*"
)

$ErrorActionPreference = "Stop"

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName WindowsBase

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

Write-Output "RangeFromPoint line read: PASS"
