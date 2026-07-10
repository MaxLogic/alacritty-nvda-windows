param(
    [string]$ProcessName = "alacritty",
    [string]$TitlePattern = "*",
    [int]$ObservationSeconds = 3,
    [int]$MaxTextChangedEvents = 90,
    [int]$MaxNotificationEvents = 90,
    [int]$MinNotificationEvents = 0,
    [string]$TracePath = "",
    [switch]$VerifyFocusPolicy,
    [int]$PhaseSeconds = 3,
    [string]$StimulusDirectory = ""
)

$ErrorActionPreference = "Stop"

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

if ($VerifyFocusPolicy) {
    Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class AlacrittyFocusProbeNative {
    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hwnd);
}
"@
}

function Get-TraceCount {
    param([string]$Path)

    $lines = @()
    if (-not [string]::IsNullOrEmpty($Path) -and (Test-Path -LiteralPath $Path)) {
        $lines = @(Get-Content -LiteralPath $Path)
    }

    [pscustomobject]@{
        TextChanged = @($lines | Where-Object { $_ -eq "20015" }).Count
        Notification = @($lines | Where-Object { $_ -eq "20035" }).Count
    }
}

function Invoke-StimulusPhase {
    param(
        [string]$Phase,
        [string]$Directory
    )

    if ([string]::IsNullOrEmpty($Directory)) {
        return
    }

    New-Item -ItemType Directory -Path $Directory -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $Directory "$Phase.signal") -Value $Phase
}

function Measure-Phase {
    param(
        [System.Windows.Automation.TextPattern]$Pattern,
        [string]$Phase,
        [int]$Seconds,
        [string]$EventTracePath,
        [string]$SignalDirectory
    )

    $beforeText = $Pattern.DocumentRange.GetText(-1)
    $beforeCounts = Get-TraceCount -Path $EventTracePath
    Invoke-StimulusPhase -Phase $Phase -Directory $SignalDirectory
    Start-Sleep -Seconds $Seconds
    $afterText = $Pattern.DocumentRange.GetText(-1)
    $afterCounts = Get-TraceCount -Path $EventTracePath

    [pscustomobject]@{
        TextChanged = $afterCounts.TextChanged - $beforeCounts.TextChanged
        Notification = $afterCounts.Notification - $beforeCounts.Notification
        ContentChanged = $beforeText -ne $afterText
    }
}

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
    param($automationSender, $automationEventArgs)
    $null = $automationSender
    $null = $automationEventArgs
    $script:TextChangedEvents += 1
}

[System.Windows.Automation.Automation]::AddAutomationEventHandler(
    [System.Windows.Automation.TextPattern]::TextChangedEvent,
    $window,
    [System.Windows.Automation.TreeScope]::Element,
    $handler
)

if ($VerifyFocusPolicy) {
    try {
        $targetHandle = [IntPtr]$window.Current.NativeWindowHandle
        $alternateHandle = [AlacrittyFocusProbeNative]::GetForegroundWindow()
        if ($alternateHandle -eq [IntPtr]::Zero -or $alternateHandle -eq $targetHandle) {
            $alternate = Get-Process | Where-Object {
                $_.MainWindowHandle -ne 0 -and $_.MainWindowHandle -ne $targetHandle
            } | Select-Object -First 1
            if ($null -eq $alternate -or $alternate.MainWindowHandle -eq 0) {
                Write-Error "No alternate top-level window is available for the unfocused phase."
            }
            $alternateHandle = $alternate.MainWindowHandle
        }

        $window.SetFocus()
        if (-not [AlacrittyFocusProbeNative]::SetForegroundWindow($targetHandle)) {
            Write-Error "Could not focus the target Alacritty window."
        }
        Start-Sleep -Milliseconds 250
        if ([AlacrittyFocusProbeNative]::GetForegroundWindow() -ne $targetHandle) {
            Write-Error "Target Alacritty window did not become foreground."
        }
        $focused = Measure-Phase -Pattern $pattern -Phase "focused" -Seconds $PhaseSeconds `
            -EventTracePath $TracePath -SignalDirectory $StimulusDirectory
        $requiredFocusedNotifications = [Math]::Max(1, $MinNotificationEvents)
        if (
            -not $focused.ContentChanged `
                -or $focused.Notification -lt $requiredFocusedNotifications
        ) {
            Write-Error "Focused output did not produce changing text and at least $requiredFocusedNotifications notification events."
        }

        if (-not [AlacrittyFocusProbeNative]::SetForegroundWindow($alternateHandle)) {
            Write-Error "Could not move focus away from the target Alacritty window."
        }
        Start-Sleep -Milliseconds 250
        if ([AlacrittyFocusProbeNative]::GetForegroundWindow() -eq $targetHandle) {
            Write-Error "Target Alacritty window remained foreground during the unfocused phase."
        }
        $unfocused = Measure-Phase -Pattern $pattern -Phase "unfocused" -Seconds $PhaseSeconds `
            -EventTracePath $TracePath -SignalDirectory $StimulusDirectory
        if (-not $unfocused.ContentChanged -or $unfocused.TextChanged -le 0) {
            Write-Error "Unfocused output did not update text and produce structural event 20015."
        }
        if ($unfocused.Notification -ne 0) {
            Write-Error "Unfocused output produced $($unfocused.Notification) notification events."
        }

        $window.SetFocus()
        if (-not [AlacrittyFocusProbeNative]::SetForegroundWindow($targetHandle)) {
            Write-Error "Could not refocus the target Alacritty window."
        }
        Start-Sleep -Milliseconds 250
        if ([AlacrittyFocusProbeNative]::GetForegroundWindow() -ne $targetHandle) {
            Write-Error "Target Alacritty window did not become foreground after refocus."
        }
        $refocused = Measure-Phase -Pattern $pattern -Phase "refocused" -Seconds $PhaseSeconds `
            -EventTracePath $TracePath -SignalDirectory $StimulusDirectory
        if ($refocused.ContentChanged) {
            Write-Error "Refocus phase must remain quiet to test background notification replay."
        }
        if ($refocused.Notification -ne 0) {
            Write-Error "Refocus replayed $($refocused.Notification) background notifications."
        }

        Write-Output "Focus speech policy: PASS"
        return
    } finally {
        [System.Windows.Automation.Automation]::RemoveAutomationEventHandler(
            [System.Windows.Automation.TextPattern]::TextChangedEvent,
            $window,
            $handler
        )
    }
}

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

if ($traceNotificationEventsObserved -lt $MinNotificationEvents) {
    Write-Error "Observed $traceNotificationEventsObserved Notification events; expected at least $MinNotificationEvents."
}

Write-Output "Event coalescing: PASS"
