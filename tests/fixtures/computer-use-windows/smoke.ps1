param(
    [string]$ExePath = "",
    [switch]$NoBuild
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$fixtureRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$buildScript = Join-Path $fixtureRoot "build.ps1"
$windowTitle = "Cortex Computer Use Fixture"
$process = $null
$startedProcesses = [System.Collections.Generic.List[System.Diagnostics.Process]]::new()

function Wait-Until {
    param(
        [scriptblock]$Condition,
        [int]$TimeoutMilliseconds,
        [string]$FailureMessage
    )

    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    while ($stopwatch.ElapsedMilliseconds -lt $TimeoutMilliseconds) {
        if (& $Condition) {
            return
        }
        Start-Sleep -Milliseconds 50
    }

    throw $FailureMessage
}

function Find-ByAutomationId {
    param(
        [Windows.Automation.AutomationElement]$Root,
        [int]$AutomationId
    )

    $condition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::AutomationIdProperty,
        $AutomationId.ToString([System.Globalization.CultureInfo]::InvariantCulture)
    )
    return $Root.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
}

function Start-ExactFixture {
    param([string]$Arguments)

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $ExePath
    $startInfo.Arguments = $Arguments
    $startInfo.UseShellExecute = $false

    $started = [System.Diagnostics.Process]::new()
    $started.StartInfo = $startInfo
    if (-not $started.Start()) {
        throw "fixture process did not start"
    }
    $startedProcesses.Add($started)
    return $started
}

try {
    if (-not $NoBuild) {
        & $buildScript
        if ($LASTEXITCODE -ne 0) {
            throw "fixture build failed with exit code $LASTEXITCODE"
        }
    }

    if ([string]::IsNullOrWhiteSpace($ExePath)) {
        $ExePath = Join-Path $fixtureRoot "target\cortex-computer-use-windows-fixture.exe"
    }
    $ExePath = [System.IO.Path]::GetFullPath($ExePath)
    if (-not (Test-Path -LiteralPath $ExePath -PathType Leaf)) {
        throw "fixture executable not found: $ExePath"
    }

    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes

    $initialStatus = "fixture status $PID"
    $copiedStatus = "fixture message $PID"
    $process = Start-ExactFixture `
        -Arguments "--status `"$initialStatus`" --exit-timeout-ms 30000"

    Wait-Until -TimeoutMilliseconds 10000 -FailureMessage "fixture window did not appear" -Condition {
        $process.Refresh()
        return (-not $process.HasExited) -and
            ($process.MainWindowHandle -ne [IntPtr]::Zero) -and
            ($process.MainWindowTitle -eq $windowTitle)
    }

    $window = [Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle)
    Wait-Until -TimeoutMilliseconds 5000 -FailureMessage "fixture UIA children did not appear" -Condition {
        return $window.FindAll(
            [Windows.Automation.TreeScope]::Descendants,
            [Windows.Automation.Condition]::TrueCondition
        ).Count -ge 8
    }
    if ($window.Current.Name -ne $windowTitle) {
        throw "window UIA name mismatch: '$($window.Current.Name)'"
    }
    if ($window.Current.IsOffscreen) {
        throw "fixture window is offscreen"
    }
    $message = Find-ByAutomationId -Root $window -AutomationId 1001
    $button = Find-ByAutomationId -Root $window -AutomationId 1002
    $password = Find-ByAutomationId -Root $window -AutomationId 1003
    $status = Find-ByAutomationId -Root $window -AutomationId 1004
    if ($null -eq $message -or $null -eq $button -or $null -eq $password -or $null -eq $status) {
        throw "one or more required UIA controls are missing"
    }
    if ($message.Current.ControlType -ne [Windows.Automation.ControlType]::Edit) {
        throw "message input is not a UIA Edit control"
    }
    if ($button.Current.ControlType -ne [Windows.Automation.ControlType]::Button) {
        throw "copy control is not a UIA Button"
    }
    if ($password.Current.ControlType -ne [Windows.Automation.ControlType]::Edit) {
        throw "password input is not a UIA Edit control"
    }
    if ($status.Current.ControlType -ne [Windows.Automation.ControlType]::Edit) {
        throw "verification status is not a read-only UIA Edit control"
    }
    foreach ($control in @($message, $button, $password, $status)) {
        if ($control.Current.IsOffscreen) {
            throw "required UIA control is offscreen: $($control.Current.AutomationId)"
        }
    }
    if ($password.Current.IsPassword -ne $true) {
        throw "password edit must expose UIA IsPassword=true"
    }
    if ($password.Current.Name -notmatch "Password") {
        throw "password edit accessible name must identify Password; got '$($password.Current.Name)'"
    }
    if ($password.Current.ItemStatus -notmatch "Password") {
        throw "password edit ItemStatus must identify Password; got '$($password.Current.ItemStatus)'"
    }
    $statusName = $status.GetCurrentPropertyValue(
        [Windows.Automation.AutomationElement]::NameProperty
    )
    $statusItemStatus = $status.GetCurrentPropertyValue(
        [Windows.Automation.AutomationElement]::ItemStatusProperty
    )
    if ($statusName -ne $initialStatus) {
        throw "initial status mismatch: expected '$initialStatus', got '$statusName'"
    }
    if ($statusItemStatus -ne $initialStatus) {
        throw "initial ItemStatus mismatch: expected '$initialStatus', got '$statusItemStatus'"
    }

    $valuePattern = [Windows.Automation.ValuePattern]$message.GetCurrentPattern(
        [Windows.Automation.ValuePattern]::Pattern
    )
    $valuePattern.SetValue($copiedStatus)
    $invokePattern = [Windows.Automation.InvokePattern]$button.GetCurrentPattern(
        [Windows.Automation.InvokePattern]::Pattern
    )
    $invokePattern.Invoke()

    Wait-Until -TimeoutMilliseconds 5000 -FailureMessage "verification status did not update" -Condition {
        $currentStatus = Find-ByAutomationId -Root $window -AutomationId 1004
        return $null -ne $currentStatus -and
            $currentStatus.GetCurrentPropertyValue(
                [Windows.Automation.AutomationElement]::NameProperty
            ) -eq $copiedStatus -and
            $currentStatus.GetCurrentPropertyValue(
                [Windows.Automation.AutomationElement]::ItemStatusProperty
            ) -eq $copiedStatus
    }

    $timeoutProcess = Start-ExactFixture `
        -Arguments "--status timeout-check --exit-timeout-ms 250"
    if (-not $timeoutProcess.WaitForExit(5000)) {
        throw "fixture ignored --exit-timeout-ms"
    }
    if ($timeoutProcess.ExitCode -ne 0) {
        throw "timed fixture exited with code $($timeoutProcess.ExitCode)"
    }

    $invalidProcess = Start-ExactFixture -Arguments "--exit-timeout-ms 0"
    if (-not $invalidProcess.WaitForExit(3000)) {
        throw "fixture accepted an invalid timeout"
    }
    if ($invalidProcess.ExitCode -ne 2) {
        throw "invalid arguments returned $($invalidProcess.ExitCode), expected 2"
    }

    Write-Output "PASS exact executable launched: $ExePath"
    Write-Output "PASS process $($process.Id), window '$windowTitle'"
    Write-Output "PASS message ValuePattern -> button InvokePattern -> status '$copiedStatus'"
    Write-Output "PASS password UIA IsPassword=true and accessible name '$($password.Current.Name)'"
    Write-Output "PASS --exit-timeout-ms exited cleanly and invalid values returned code 2"
}
catch {
    Write-Error $_
    exit 1
}
finally {
    foreach ($started in $startedProcesses) {
        if (-not $started.HasExited) {
            $null = $started.CloseMainWindow()
            if (-not $started.WaitForExit(3000)) {
                $started.Kill()
                $null = $started.WaitForExit(3000)
            }
        }
        $started.Dispose()
    }
}
