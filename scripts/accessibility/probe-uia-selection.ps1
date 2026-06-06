param(
    [string]$ProcessName = "alacritty",
    [string]$TitlePattern = "*Codex UIA Selection Probe*"
)

$ErrorActionPreference = "Stop"

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

Add-Type @"
using System;
using System.Runtime.InteropServices;

public static class NativeInput {
    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int x, int y);

    [DllImport("user32.dll")]
    public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hwnd);
}
"@

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

$process = Get-Process -Id $window.Current.ProcessId -ErrorAction Stop
[NativeInput]::SetForegroundWindow($process.MainWindowHandle) | Out-Null
Start-Sleep -Milliseconds 300

$rect = $window.Current.BoundingRectangle
if ($rect.IsEmpty -or $rect.Width -le 0 -or $rect.Height -le 0) {
    Write-Error "Window bounding rectangle is empty."
}

$startX = [int]($rect.Left + 20)
$startY = [int]($rect.Top + 45)
$endX = [int]($rect.Left + 260)
$endY = $startY

[NativeInput]::SetCursorPos($startX, $startY) | Out-Null
Start-Sleep -Milliseconds 100
[NativeInput]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 100
[NativeInput]::SetCursorPos($endX, $endY) | Out-Null
Start-Sleep -Milliseconds 200
[NativeInput]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 500

$pattern = $null
if (-not $window.TryGetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern, [ref]$pattern)) {
    Write-Error "TextPattern is not available for '$($window.Current.Name)'."
}

$selection = $pattern.GetSelection()
if ($null -eq $selection -or $selection.Count -lt 1) {
    Write-Error "GetSelection returned no selected ranges."
}

$text = $selection[0].GetText(-1)
if ([string]::IsNullOrWhiteSpace($text)) {
    Write-Error "Selected range text was empty."
}

if ($text -like "*second selectable line*" -or $text -like "*third selectable line*") {
    Write-Error "Selected range looks like stale or full-document text: '$text'."
}

if ($text -notmatch "electable|selectable") {
    Write-Error "Selected range did not include expected probe text: '$text'."
}

Write-Output "Selection: PASS"
