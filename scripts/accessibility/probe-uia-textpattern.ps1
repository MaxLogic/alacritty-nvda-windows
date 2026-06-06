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

Write-Output "TextPattern.Available: PASS"
Write-Output "DocumentRange.GetText: PASS"
Write-Output "GetVisibleRanges: PASS"
Write-Output "GetSelection: PASS"
