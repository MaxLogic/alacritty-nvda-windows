param(
    [string]$ProcessName = "alacritty",
    [string]$TitlePattern = "*"
)

$ErrorActionPreference = "Stop"

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

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

$range = $pattern.DocumentRange.Clone()
$range.ExpandToEnclosingUnit([System.Windows.Automation.Text.TextUnit]::Line)
$firstLine = $range.GetText(-1)
if ([string]::IsNullOrWhiteSpace($firstLine)) {
    Write-Error "First line navigation returned no text."
}

$moved = $range.Move([System.Windows.Automation.Text.TextUnit]::Line, 1)
if ($moved -lt 0) {
    Write-Error "Line navigation moved in the wrong direction."
}

if ($moved -gt 0) {
    $nextLine = $range.GetText(-1)
    if ([string]::IsNullOrWhiteSpace($nextLine)) {
        Write-Error "Next line navigation returned no text."
    }
}

Write-Output "Line navigation: PASS"
