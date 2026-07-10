param(
    [string]$AlacrittyPath = ""
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($AlacrittyPath)) {
    $AlacrittyPath = Join-Path $PSScriptRoot `
        "..\..\target\x86_64-pc-windows-msvc\debug\alacritty.exe"
}
$AlacrittyPath = [IO.Path]::GetFullPath($AlacrittyPath)
if (-not (Test-Path -LiteralPath $AlacrittyPath -PathType Leaf)) {
    Write-Error "Alacritty executable not found at '$AlacrittyPath'."
}

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class AlacrittyControlJProbeNative {
    private const byte VkControl = 0x11;
    private const byte VkJ = 0x4A;
    private const uint KeyboardInput = 1;
    private const uint KeyUp = 0x0002;
    private const int Restore = 9;

    // This probe targets the x86_64 build. Native INPUT is 40 bytes there:
    // a 4-byte type, 4 bytes of alignment, and a 32-byte union.
    [StructLayout(LayoutKind.Explicit, Size = 40)]
    private struct Input {
        [FieldOffset(0)] public uint Type;
        [FieldOffset(8)]
        public KeyboardInputData Keyboard;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct KeyboardInputData {
        public ushort VirtualKey;
        public ushort ScanCode;
        public uint Flags;
        public uint Time;
        public UIntPtr ExtraInfo;
    }

    [DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr hwnd);

    [DllImport("user32.dll")]
    private static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);

    [DllImport("kernel32.dll")]
    private static extern uint GetCurrentThreadId();

    [DllImport("user32.dll")]
    private static extern bool AttachThreadInput(uint first, uint second, bool attach);

    [DllImport("user32.dll")]
    private static extern bool ShowWindow(IntPtr hwnd, int command);

    [DllImport("user32.dll")]
    private static extern bool BringWindowToTop(IntPtr hwnd);

    [DllImport("user32.dll")]
    private static extern IntPtr SetActiveWindow(IntPtr hwnd);

    [DllImport("user32.dll")]
    private static extern IntPtr SetFocus(IntPtr hwnd);

    [DllImport("user32.dll")]
    private static extern short GetAsyncKeyState(int virtualKey);

    [DllImport("user32.dll")]
    private static extern short VkKeyScanW(char character);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint SendInput(uint count, Input[] inputs, int size);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowTextW(IntPtr hwnd, char[] text, int count);

    private static Input Key(byte virtualKey, uint flags) {
        return new Input {
            Type = KeyboardInput,
            Keyboard = new KeyboardInputData {
                VirtualKey = virtualKey,
                Flags = flags
            }
        };
    }

    public static bool FocusWindow(IntPtr hwnd) {
        ShowWindow(hwnd, Restore);
        IntPtr foreground = GetForegroundWindow();
        uint ignored;
        uint foregroundThread = GetWindowThreadProcessId(foreground, out ignored);
        uint targetThread = GetWindowThreadProcessId(hwnd, out ignored);
        uint currentThread = GetCurrentThreadId();
        bool foregroundAttached = foregroundThread != 0
            && foregroundThread != currentThread
            && AttachThreadInput(currentThread, foregroundThread, true);
        bool targetAttached = targetThread != 0
            && targetThread != currentThread
            && targetThread != foregroundThread
            && AttachThreadInput(currentThread, targetThread, true);
        try {
            BringWindowToTop(hwnd);
            SetActiveWindow(hwnd);
            SetFocus(hwnd);
            SetForegroundWindow(hwnd);
            return GetForegroundWindow() == hwnd;
        }
        finally {
            if (targetAttached) {
                AttachThreadInput(currentThread, targetThread, false);
            }
            if (foregroundAttached) {
                AttachThreadInput(currentThread, foregroundThread, false);
            }
        }
    }

    public static bool KeysAreUp() {
        return (GetAsyncKeyState(VkControl) & 0x8000) == 0
            && (GetAsyncKeyState(VkJ) & 0x8000) == 0;
    }

    public static bool LayoutMapsJ() {
        return (VkKeyScanW('j') & 0xff) == VkJ;
    }

    public static void SendControlJPressRepeatRelease() {
        Input[] inputs = {
            Key(VkControl, 0),
            Key(VkJ, 0),
            Key(VkJ, 0),
            Key(VkJ, KeyUp),
            Key(VkControl, KeyUp)
        };
        uint sent = SendInput((uint)inputs.Length, inputs, Marshal.SizeOf(typeof(Input)));
        if (sent != inputs.Length) {
            int error = Marshal.GetLastWin32Error();
            ReleaseKeys();
            throw new InvalidOperationException(
                "SendInput sent " + sent + " of " + inputs.Length + " events; "
                + "size=" + Marshal.SizeOf(typeof(Input)) + ", error=" + error + "."
            );
        }
    }

    public static void ReleaseKeys() {
        Input[] inputs = { Key(VkJ, KeyUp), Key(VkControl, KeyUp) };
        SendInput((uint)inputs.Length, inputs, Marshal.SizeOf(typeof(Input)));
    }

    public static string WindowText(IntPtr hwnd) {
        char[] text = new char[512];
        int length = GetWindowTextW(hwnd, text, text.Length);
        return new string(text, 0, Math.Max(0, length));
    }
}
"@

$tempDirectory = Join-Path $env:TEMP ("alacritty-control-j-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tempDirectory | Out-Null
$probeProcesses = [Collections.Generic.List[Diagnostics.Process]]::new()
$childPath = Join-Path $tempDirectory "capture-input.ps1"

@'
param(
    [string]$OutputPath,
    [string]$ReadyPath,
    [string]$ReadyTitle,
    [int]$ExpectedCount,
    [int]$ModeFlags
)

$ErrorActionPreference = "Stop"
Add-Type -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

public static class AlacrittyRawInputCapture {
    private const int StdInput = -10;
    private const uint ProcessedInput = 0x0001;
    private const uint LineInput = 0x0002;
    private const uint EchoInput = 0x0004;
    private const uint VirtualTerminalInput = 0x0200;
    private const uint WaitObject0 = 0;

    public static IDisposable ArmExitWatchdog(int timeoutMilliseconds) {
        return new System.Threading.Timer(
            _ => Environment.Exit(124),
            null,
            timeoutMilliseconds,
            System.Threading.Timeout.Infinite
        );
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr GetStdHandle(int handle);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetConsoleMode(IntPtr handle, out uint mode);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetConsoleMode(IntPtr handle, uint mode);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool ReadFile(
        IntPtr handle,
        byte[] buffer,
        uint count,
        out uint read,
        IntPtr overlapped
    );

    public static byte[] Read(int expectedCount, int timeoutMilliseconds) {
        IntPtr input = GetStdHandle(StdInput);
        uint originalMode;
        if (!GetConsoleMode(input, out originalMode)) {
            throw new InvalidOperationException("GetConsoleMode failed.");
        }
        uint rawMode = (originalMode & ~(ProcessedInput | LineInput | EchoInput))
            | VirtualTerminalInput;
        if (!SetConsoleMode(input, rawMode)) {
            throw new InvalidOperationException("SetConsoleMode failed.");
        }

        var bytes = new List<byte>(expectedCount);
        DateTime deadline = DateTime.UtcNow.AddMilliseconds(timeoutMilliseconds);
        try {
            while (bytes.Count < expectedCount && DateTime.UtcNow < deadline) {
                int remaining = Math.Max(1, (int)(deadline - DateTime.UtcNow).TotalMilliseconds);
                if (WaitForSingleObject(input, (uint)remaining) != WaitObject0) {
                    break;
                }
                byte[] buffer = new byte[expectedCount - bytes.Count];
                uint read;
                if (!ReadFile(input, buffer, (uint)buffer.Length, out read, IntPtr.Zero)) {
                    throw new InvalidOperationException("ReadFile failed.");
                }
                for (int index = 0; index < read; index++) {
                    bytes.Add(buffer[index]);
                }
            }
        }
        finally {
            SetConsoleMode(input, originalMode);
        }
        return bytes.ToArray();
    }
}
"@

$stdout = [Console]::OpenStandardOutput()
$setup = ""
if ($ModeFlags -gt 0) {
    $setup += "{0}[>{1}u" -f [char]27, $ModeFlags
}
$setup += "{0}]2;{1}{2}" -f [char]27, $ReadyTitle, [char]7
$setupBytes = [Text.Encoding]::ASCII.GetBytes($setup)
$stdout.Write($setupBytes, 0, $setupBytes.Length)
$stdout.Flush()

[IO.File]::WriteAllText($ReadyPath, "ready")
$watchdog = [AlacrittyRawInputCapture]::ArmExitWatchdog(15000)
try {
    $captured = [AlacrittyRawInputCapture]::Read($ExpectedCount, 10000)
    [IO.File]::WriteAllBytes($OutputPath, $captured)
    if ($captured.Length -ne $ExpectedCount) {
        exit 2
    }
}
finally {
    $watchdog.Dispose()
}
'@ | Set-Content -LiteralPath $childPath -Encoding UTF8
$configPath = Join-Path $tempDirectory "alacritty.toml"
Set-Content -LiteralPath $configPath -Value "" -Encoding UTF8

function Invoke-ControlJPhase {
    param(
        [string]$Name,
        [int]$ModeFlags,
        [byte[]]$ExpectedBytes
    )

    $id = [guid]::NewGuid().ToString("N")
    $readyTitle = "T012-CtrlJ-READY-$Name-$id"
    $capturePath = Join-Path $tempDirectory "$Name.bin"
    $readyPath = Join-Path $tempDirectory "$Name.ready"
    $arguments = @(
        "--config-file", $configPath,
        "-o", "window.dynamic_title=true",
        "-e", "powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass",
        "-File", $childPath,
        "-OutputPath", $capturePath,
        "-ReadyPath", $readyPath,
        "-ReadyTitle", $readyTitle,
        "-ExpectedCount", $ExpectedBytes.Length,
        "-ModeFlags", $ModeFlags
    )
    $process = Start-Process -FilePath $AlacrittyPath -ArgumentList $arguments -PassThru
    $probeProcesses.Add($process)

    try {
        $deadline = [DateTime]::UtcNow.AddSeconds(30)
        do {
            Start-Sleep -Milliseconds 100
            $liveProcess = Get-Process -Id $process.Id -ErrorAction SilentlyContinue
        } while (
            ($null -eq $liveProcess `
                -or $liveProcess.MainWindowHandle -eq 0 `
                -or -not (Test-Path -LiteralPath $readyPath) `
                -or [AlacrittyControlJProbeNative]::WindowText(
                    [IntPtr]$liveProcess.MainWindowHandle
                ) -ne $readyTitle) `
            -and [DateTime]::UtcNow -lt $deadline
        )
        if (
            $null -eq $liveProcess `
                -or $liveProcess.MainWindowHandle -eq 0 `
                -or -not (Test-Path -LiteralPath $readyPath) `
                -or [AlacrittyControlJProbeNative]::WindowText(
                    [IntPtr]$liveProcess.MainWindowHandle
                ) -ne $readyTitle
        ) {
            $observedTitle = if ($null -ne $liveProcess -and $liveProcess.MainWindowHandle -ne 0) {
                [AlacrittyControlJProbeNative]::WindowText(
                    [IntPtr]$liveProcess.MainWindowHandle
                )
            } else {
                "<no-window>"
            }
            Write-Error (
                "$Name phase did not become ready; " `
                    + "process=$($null -ne $liveProcess), " `
                    + "marker=$(Test-Path -LiteralPath $readyPath), " `
                    + "title='$observedTitle', expected='$readyTitle'."
            )
        }

        if (-not [AlacrittyControlJProbeNative]::LayoutMapsJ()) {
            Write-Error "The active keyboard layout does not map 'j' to VK_J."
        }
        if (-not [AlacrittyControlJProbeNative]::KeysAreUp()) {
            Write-Error "Ctrl or J was already pressed before the $Name phase."
        }
        if (-not [AlacrittyControlJProbeNative]::FocusWindow(
            [IntPtr]$liveProcess.MainWindowHandle
        )) {
            Write-Error "Could not focus the dedicated $Name test window."
        }
        Start-Sleep -Milliseconds 250
        [AlacrittyControlJProbeNative]::SendControlJPressRepeatRelease()

        if (-not $process.WaitForExit(15000)) {
            Write-Error "$Name phase did not exit after its child timeout."
        }
        if ($process.ExitCode -ne 0) {
            Write-Error "$Name phase exited with code $($process.ExitCode)."
        }

        $actualBytes = [IO.File]::ReadAllBytes($capturePath)
        $actualHex = [Convert]::ToHexString($actualBytes)
        $expectedHex = [Convert]::ToHexString($ExpectedBytes)
        if ($actualHex -ne $expectedHex) {
            Write-Error "$Name bytes '$actualHex' did not match '$expectedHex'."
        }
    }
    finally {
        [AlacrittyControlJProbeNative]::ReleaseKeys()
    }
}

try {
    Invoke-ControlJPhase -Name "normal" -ModeFlags 0 -ExpectedBytes ([byte[]](10, 10))

    $escape = [char]27
    $kittyText = "${escape}[106;5u${escape}[106;5:2u${escape}[106;5:3u"
    $kittyBytes = [Text.Encoding]::ASCII.GetBytes($kittyText)
    Invoke-ControlJPhase -Name "disambiguate" -ModeFlags 3 -ExpectedBytes $kittyBytes
    $reportAllText = "${escape}[57442;5u$kittyText${escape}[57442;1:3u"
    $reportAllBytes = [Text.Encoding]::ASCII.GetBytes($reportAllText)
    Invoke-ControlJPhase -Name "report-all" -ModeFlags 10 -ExpectedBytes $reportAllBytes

    Write-Output "Ctrl+J press/repeat normal LF: PASS"
    Write-Output "Ctrl+J release suppression: PASS"
    Write-Output "Kitty disambiguate press/repeat/release encoding: PASS"
    Write-Output "Kitty report-all press/repeat/release encoding: PASS"
}
finally {
    if (-not ($probeProcesses | Where-Object { -not $_.HasExited })) {
        Remove-Item -LiteralPath $tempDirectory -Recurse -Force -ErrorAction SilentlyContinue
    }
}
