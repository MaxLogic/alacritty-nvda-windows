param(
    [string]$ProcessName = "alacritty",
    [string]$TitlePattern = "*",
    [int]$ObservationSeconds = 3,
    [int]$MaxTextChangedEvents = 90,
    [int]$MaxNotificationEvents = 90,
    [string]$TracePath = ""
)

$ErrorActionPreference = "Stop"

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

if ([string]::IsNullOrEmpty($TracePath)) {
    $TracePath = $env:ALACRITTY_UIA_EVENT_TRACE
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

$script:TextChangedEvents = 0
$traceLinesBefore = @()
if (-not [string]::IsNullOrEmpty($TracePath) -and (Test-Path -LiteralPath $TracePath)) {
    $traceLinesBefore = @(Get-Content -LiteralPath $TracePath)
}
$traceTextEventsBefore = @($traceLinesBefore | Where-Object { $_ -eq "20015" }).Count
$traceNotificationEventsBefore = @($traceLinesBefore | Where-Object { $_ -eq "20035" }).Count

$handler = [System.Windows.Automation.AutomationEventHandler]{
    param($sender, $eventArgs)
    $script:TextChangedEvents += 1
}

[System.Windows.Automation.Automation]::AddAutomationEventHandler(
    [System.Windows.Automation.TextPattern]::TextChangedEvent,
    $window,
    [System.Windows.Automation.TreeScope]::Element,
    $handler
)

try {
    $beforeText = $pattern.DocumentRange.GetText(-1)
    Start-Sleep -Seconds $ObservationSeconds
    $afterText = $pattern.DocumentRange.GetText(-1)
} finally {
    [System.Windows.Automation.Automation]::RemoveAutomationEventHandler(
        [System.Windows.Automation.TextPattern]::TextChangedEvent,
        $window,
        $handler
    )
}

if ($beforeText -eq $afterText) {
    Write-Error "Document text did not change during the observation window."
}

$traceLines = @()
if (-not [string]::IsNullOrEmpty($TracePath) -and (Test-Path -LiteralPath $TracePath)) {
    $traceLines = @(Get-Content -LiteralPath $TracePath)
}

$traceTextEvents = @($traceLines | Where-Object { $_ -eq "20015" })
$traceNotificationEvents = @($traceLines | Where-Object { $_ -eq "20035" })
$traceEventsObserved = [Math]::Max(0, $traceTextEvents.Count - $traceTextEventsBefore)
$traceNotificationEventsObserved = [Math]::Max(
    0,
    $traceNotificationEvents.Count - $traceNotificationEventsBefore
)
$observedEvents = [Math]::Max($script:TextChangedEvents, $traceEventsObserved)

if ($observedEvents -le 0) {
    Write-Error "No TextChanged events observed."
}

if ($observedEvents -gt $MaxTextChangedEvents) {
    Write-Error "Observed $observedEvents TextChanged events; expected at most $MaxTextChangedEvents."
}

if ($traceNotificationEventsObserved -gt $MaxNotificationEvents) {
    Write-Error "Observed $traceNotificationEventsObserved Notification events; expected at most $MaxNotificationEvents."
}

Write-Output "Event coalescing: PASS"
