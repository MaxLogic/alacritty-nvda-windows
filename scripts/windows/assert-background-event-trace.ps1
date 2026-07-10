param(
    [string]$TraceDirectory = "",
    [long]$MaxInternalTicks = [long]::MaxValue,
    [long]$MaxObserverTicks = [long]::MaxValue,
    [switch]$RequireUia
)

$ErrorActionPreference = "Stop"

function Test-BackgroundEventTrace {
    param(
        [Parameter(Mandatory)]
        [string]$TraceDirectory,
        [long]$MaxInternalTicks = [long]::MaxValue,
        [long]$MaxObserverTicks = [long]::MaxValue,
        [switch]$RequireUia
    )

    $records = Get-ChildItem -LiteralPath $TraceDirectory -Filter "*.jsonl" -File |
        ForEach-Object { Get-Content -LiteralPath $_.FullName } |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
        ForEach-Object { $_ | ConvertFrom-Json }
    if (-not $records) {
        throw "No trace records found in '$TraceDirectory'."
    }

    $exactStages = @("producer", "pty_read", "parser_title", "enqueue", "winit_receipt", "title_apply")

    $sequenceGroups = @($records | Group-Object run, seq | Sort-Object Name)
    $groupsByKey = @{}
    $maximumByRun = @{}
    foreach ($group in $sequenceGroups) {
        $run = [string]$group.Group[0].run
        $sequenceNumber = [long]$group.Group[0].seq
        $groupsByKey["$run|$sequenceNumber"] = $group.Group
        if (-not $maximumByRun.ContainsKey($run) -or $sequenceNumber -gt $maximumByRun[$run]) {
            $maximumByRun[$run] = $sequenceNumber
        }
    }

    foreach ($sequenceGroup in $sequenceGroups) {
        $sequence = $sequenceGroup.Group
        $byStage = @{}
        foreach ($record in ($sequence | Sort-Object ticks)) {
            if (-not $byStage.ContainsKey($record.stage)) {
                $byStage[$record.stage] = $record
            }
        }

        $previous = $null
        foreach ($stage in $exactStages) {
            if (-not $byStage.ContainsKey($stage)) {
                return [pscustomobject]@{
                    Status = "stalled"
                    Run = $sequence[0].run
                    Sequence = [long]$sequence[0].seq
                    FirstStalledStage = $stage
                    DelayTicks = $null
                }
            }
            $current = $byStage[$stage]
            if ($null -ne $previous) {
                $delay = [long]$current.ticks - [long]$previous.ticks
                if ($delay -gt $MaxInternalTicks) {
                    return [pscustomobject]@{
                        Status = "stalled"
                        Run = $sequence[0].run
                        Sequence = [long]$sequence[0].seq
                        FirstStalledStage = $stage
                        DelayTicks = $delay
                    }
                }
            }
            $previous = $current
        }

        $coalescedPaths = @(
                @{ Previous = "title_apply"; Stages = @("native_title") },
                @{
                    Previous = "winit_receipt"
                    Stages = @("terminal_wakeup", "redraw_request", "redraw_delivery", "frame_complete")
                }
            )
        if ($RequireUia) {
            $coalescedPaths += @{ Previous = "winit_receipt"; Stages = @("uia_content") }
        }
        foreach ($coalescedPath in $coalescedPaths) {
            $previous = $byStage[$coalescedPath.Previous]
            foreach ($stage in $coalescedPath.Stages) {
            $run = [string]$sequence[0].run
            $candidateSequence = [long]$sequence[0].seq
            $current = $null
            while ($null -eq $current -and $candidateSequence -le $maximumByRun[$run]) {
                $candidateGroup = $groupsByKey["$run|$candidateSequence"]
                $current = $candidateGroup |
                    Where-Object { $_.stage -eq $stage -and [long]$_.ticks -ge [long]$previous.ticks } |
                    Sort-Object ticks |
                    Select-Object -First 1
                $candidateSequence++
            }
            if ($null -eq $current) {
                return [pscustomobject]@{
                    Status = "stalled"
                    Run = $sequence[0].run
                    Sequence = [long]$sequence[0].seq
                    FirstStalledStage = $stage
                    DelayTicks = $null
                }
            }
            $delay = [long]$current.ticks - [long]$previous.ticks
            $budget = if ($stage -in "native_title", "uia_content") {
                $MaxObserverTicks
            } else {
                $MaxInternalTicks
            }
            if ($delay -gt $budget) {
                return [pscustomobject]@{
                    Status = "stalled"
                    Run = $sequence[0].run
                    Sequence = [long]$sequence[0].seq
                    FirstStalledStage = $stage
                    DelayTicks = $delay
                }
            }
            $previous = $current
            }
        }
    }

    [pscustomobject]@{
        Status = "converged"
        Run = $null
        Sequence = $null
        FirstStalledStage = $null
        DelayTicks = $null
    }
}

if ($MyInvocation.InvocationName -ne ".") {
    if ([string]::IsNullOrWhiteSpace($TraceDirectory)) {
        throw "TraceDirectory is required."
    }
    $result = Test-BackgroundEventTrace @PSBoundParameters
    $result | ConvertTo-Json -Compress
    if ($result.Status -ne "converged") {
        exit 2
    }
}
