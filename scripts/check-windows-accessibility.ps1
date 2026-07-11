$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = Resolve-Path (Join-Path $scriptDir "..")
Set-Location $repoRoot

function Invoke-GateCommand {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Label,

        [Parameter(Mandatory = $true)]
        [string] $Executable,

        [Parameter(ValueFromRemainingArguments = $true)]
        [string[]] $Arguments
    )

    Write-Host ""
    Write-Host "==> $Label"
    & $Executable @Arguments

    if ($LASTEXITCODE -ne 0) {
        throw "'$Executable $($Arguments -join ' ')' failed with exit code $LASTEXITCODE."
    }
}

if (-not (rustup toolchain list | Select-String -Quiet -Pattern "^nightly")) {
    throw "nightly Rust toolchain with rustfmt is required. Install it with: rustup toolchain install nightly --profile minimal -c rustfmt"
}

Invoke-GateCommand "Check Rust formatting" cargo +nightly fmt -- --check
Invoke-GateCommand "Run Clippy for Windows MSVC" cargo clippy --all-targets --target x86_64-pc-windows-msvc
Invoke-GateCommand "Check Alacritty for Windows MSVC" cargo check -p alacritty --target x86_64-pc-windows-msvc
Invoke-GateCommand "Run terminal crate tests" cargo test -p alacritty_terminal

Write-Host ""
Write-Host "Windows accessibility gate passed."
