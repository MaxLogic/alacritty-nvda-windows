BeforeAll {
    . (Join-Path $PSScriptRoot "..\assert-background-event-trace.ps1")

    function Write-TraceFixture {
        param(
            [string]$Path,
            [string[]]$Stages
        )

        New-Item -ItemType Directory -Path $Path | Out-Null
        $tick = 1000
        foreach ($stage in $Stages) {
            @{ run = "test"; seq = 1; stage = $stage; ticks = $tick } |
                ConvertTo-Json -Compress |
                Add-Content -LiteralPath (Join-Path $Path "trace.jsonl")
            $tick += 10
        }
    }
}

Describe "Test-BackgroundEventTrace" {
    It "accepts a sequence that reaches native title and frame completion" {
        $path = Join-Path $TestDrive "converged"
        Write-TraceFixture -Path $path -Stages @(
            "producer", "pty_read", "parser_title", "enqueue", "winit_receipt",
            "title_apply", "native_title", "terminal_wakeup", "redraw_request",
            "redraw_delivery", "frame_complete"
        )

        $result = Test-BackgroundEventTrace -TraceDirectory $path

        $result.Status | Should -Be "converged"
        $result.FirstStalledStage | Should -BeNullOrEmpty
    }

    It "reports the first missing stage" {
        $path = Join-Path $TestDrive "missing"
        Write-TraceFixture -Path $path -Stages @("producer", "pty_read", "parser_title", "enqueue")

        $result = Test-BackgroundEventTrace -TraceDirectory $path

        $result.Status | Should -Be "stalled"
        $result.FirstStalledStage | Should -Be "winit_receipt"
        $result.Sequence | Should -Be 1
    }

    It "reports the first stage over its latency budget" {
        $path = Join-Path $TestDrive "delayed"
        Write-TraceFixture -Path $path -Stages @(
            "producer", "pty_read", "parser_title", "enqueue", "winit_receipt",
            "title_apply", "native_title", "terminal_wakeup", "redraw_request",
            "redraw_delivery", "frame_complete"
        )
        $trace = Join-Path $path "trace.jsonl"
        $records = Get-Content -LiteralPath $trace | ForEach-Object { $_ | ConvertFrom-Json }
        ($records | Where-Object stage -eq "winit_receipt").ticks = 2000
        $records | ForEach-Object { $_ | ConvertTo-Json -Compress } | Set-Content -LiteralPath $trace

        $result = Test-BackgroundEventTrace -TraceDirectory $path -MaxInternalTicks 100

        $result.Status | Should -Be "stalled"
        $result.FirstStalledStage | Should -Be "winit_receipt"
        $result.DelayTicks | Should -BeGreaterThan 100
    }

    It "accepts later coalesced native and frame state for an earlier sequence" {
        $path = Join-Path $TestDrive "coalesced"
        Write-TraceFixture -Path $path -Stages @(
            "producer", "pty_read", "parser_title", "enqueue", "winit_receipt",
            "title_apply"
        )
        $trace = Join-Path $path "trace.jsonl"
        $tick = 1100
        foreach ($stage in @(
                "producer", "pty_read", "parser_title", "enqueue", "winit_receipt",
                "title_apply", "native_title", "terminal_wakeup", "redraw_request",
                "redraw_delivery", "frame_complete")) {
            @{ run = "test"; seq = 2; stage = $stage; ticks = $tick } |
                ConvertTo-Json -Compress | Add-Content -LiteralPath $trace
            $tick += 10
        }

        $result = Test-BackgroundEventTrace -TraceDirectory $path

        $result.Status | Should -Be "converged"
    }

    It "requires externally observed UIA content when requested" {
        $path = Join-Path $TestDrive "uia-missing"
        Write-TraceFixture -Path $path -Stages @(
            "producer", "pty_read", "parser_title", "enqueue", "winit_receipt",
            "title_apply", "native_title", "terminal_wakeup", "redraw_request",
            "redraw_delivery", "frame_complete"
        )

        $result = Test-BackgroundEventTrace -TraceDirectory $path -RequireUia

        $result.Status | Should -Be "stalled"
        $result.FirstStalledStage | Should -Be "uia_content"
    }
}
