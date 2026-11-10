# E2E 子进程可能正常退出，但报告里仍有失败；串联验收必须核对报告本身。
function Read-E2EReport {
    param([string]$Path)
    # 报告无 BOM；Windows PowerShell 5.1 的默认 ANSI 会破坏中文及 JSON 引号。
    Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json
}

function Assert-E2EReport {
    param([object]$Report)

    if ($Report.suite -ne 'cortex-computer-use-windows-e2e' -or
        $Report.host.desktopLockedAtReport -isnot [bool] -or
        $Report.host.desktopLockedAtReport) {
        throw 'E2E report has no verified unlocked desktop.'
    }
    $tasks = @($Report.tasks)
    $passed = @($tasks | Where-Object { $_.status -eq 'pass' }).Count
    $failed = @($tasks | Where-Object { $_.status -eq 'fail' }).Count
    $skipped = @($tasks | Where-Object { $_.status -eq 'skipped' }).Count
    $ids = @($tasks | ForEach-Object { $_.id } | Sort-Object -Unique)
    if ($tasks.Count -ne 20 -or $ids.Count -ne 20 -or
        $Report.summary.total -ne 20 -or
        $passed + $failed + $skipped -ne 20 -or
        $Report.summary.pass -ne $passed -or
        $Report.summary.fail -ne $failed -or
        $Report.summary.skipped -ne $skipped) {
        throw 'E2E report is incomplete or its summary does not match task outcomes.'
    }
    if ($failed -gt 0) {
        throw "E2E report contains $failed failed task(s)."
    }
    foreach ($task in $tasks | Where-Object { $_.status -eq 'skipped' }) {
        if ($task.id -notin @('notepad-save-file', 'multi-monitor-negative-origin') -or
            [string]::IsNullOrWhiteSpace($task.reason)) {
            throw "E2E report skipped a required task: $($task.id)."
        }
    }
}
