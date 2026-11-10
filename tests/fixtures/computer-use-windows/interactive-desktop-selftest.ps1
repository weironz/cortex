param([switch]$ProbeCurrentDesktop)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$script:desktopChecks = 0
function Assert-DesktopTest {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) { throw "Interactive desktop regression: $Message" }
    $script:desktopChecks++
}

$modulePath = Join-Path $PSScriptRoot 'interactive-desktop.ps1'
$wasLoaded = $null -ne ('CortexInteractiveDesktop.Windows' -as [type])
. $modulePath
Assert-DesktopTest ($wasLoaded -or $null -eq ('CortexInteractiveDesktop.Windows' -as [type])) `
    'dot-sourcing must not initialize native desktop queries'
Initialize-InteractiveDesktopNative
Initialize-InteractiveDesktopNative

# 五个证据源都是替身；零 HWND、锁屏和错误都不靠改变本机桌面来制造。
function Invoke-SyntheticDesktopProbe {
    param([hashtable]$Case)
    $reads = [Collections.Generic.List[string]]::new()
    $current = [Func[uint32]] {
        $reads.Add('current')
        if ($Case.fault -eq 'current') { throw [ComponentModel.Win32Exception]::new(5, 'do-not-echo-query-detail') }
        return [uint32]$Case.processSession
    }
    $session = [Func[uint32,CortexInteractiveDesktop.SessionInformation]] {
        param([uint32]$RequestedSession)
        $reads.Add('session')
        if ($RequestedSession -ne $Case.processSession) { throw 'query was not bound to the calling session' }
        if ($Case.fault -eq 'session') { throw [ComponentModel.Win32Exception]::new(5, 'do-not-echo-query-detail') }
        if ($Case.nullRead -eq 'session') { return $null }
        $value = [CortexInteractiveDesktop.SessionInformation]::new()
        $value.Level = $Case.level; $value.SessionId = $Case.reportedSession
        $value.State = $Case.state; $value.Flags = $Case.flags
        return $value
    }
    $station = [Func[string]] {
        $reads.Add('station')
        if ($Case.fault -eq 'station') { throw [ComponentModel.Win32Exception]::new(5, 'do-not-echo-query-detail') }
        return $Case.station
    }
    $desktop = [Func[string]] {
        $reads.Add('desktop')
        if ($Case.fault -eq 'desktop') { throw [ComponentModel.Win32Exception]::new(5, 'do-not-echo-query-detail') }
        return $Case.desktop
    }
    $foreground = [Func[CortexInteractiveDesktop.ForegroundInformation]] {
        $reads.Add('foreground')
        if ($Case.fault -eq 'foreground') { throw [ComponentModel.Win32Exception]::new(5, 'do-not-echo-query-detail') }
        if ($Case.nullRead -eq 'foreground') { return $null }
        $value = [CortexInteractiveDesktop.ForegroundInformation]::new()
        $value.HasForeground = $Case.hasForeground; $value.SessionId = $Case.foregroundSession
        $value.ProcessName = $Case.processName
        return $value
    }
    $evidence = [CortexInteractiveDesktop.Windows]::InteractiveDesktop($current, $session, $station, $desktop, $foreground)
    return [pscustomobject]@{ evidence = $evidence; reads = $reads.ToArray() }
}

$base = @{
    processSession = 7; level = 1; reportedSession = 7; state = 0; flags = 1
    station = 'WinSta0'; desktop = 'Default'; hasForeground = $false
    foregroundSession = 7; processName = $null; fault = ''; nullRead = ''
}
$cases = [Collections.Generic.List[object]]::new()
$cases.Add(@{ value = $base.Clone(); reason = 'INTERACTIVE_DESKTOP_ALLOWED'; name = 'zero HWND with complete evidence' })
$lower = $base.Clone(); $lower.station = 'winsta0'; $lower.desktop = 'default'
$cases.Add(@{ value = $lower; reason = 'INTERACTIVE_DESKTOP_ALLOWED'; name = 'case insensitive desktop identity' })
$visible = $base.Clone(); $visible.hasForeground = $true; $visible.processName = 'fixture'
$cases.Add(@{ value = $visible; reason = 'INTERACTIVE_DESKTOP_ALLOWED'; name = 'known ordinary foreground' })
foreach ($field in @('state', 'flags', 'station', 'desktop', 'level', 'reportedSession', 'processSession')) {
    $values = switch ($field) {
        'state' { @(1, 2, 3, 4, 5, 6, 7, 8, 9, -1) }
        'flags' { @(0, 2, [uint32]::MaxValue) }
        'station' { @($null, '', 'Service-0x0') }
        'desktop' { @($null, '', 'Winlogon', 'LockApp', 'Unknown') }
        'level' { @(0, 2, [uint32]::MaxValue) }
        'reportedSession' { @(0, 8) }
        'processSession' { @(0) }
    }
    $reason = switch ($field) {
        'state' { 'SESSION_NOT_ACTIVE' }
        'flags' { 'SESSION_NOT_UNLOCKED' }
        'station' { 'WINDOW_STATION_NOT_INTERACTIVE' }
        'desktop' { 'INPUT_DESKTOP_NOT_DEFAULT' }
        'level' { 'SESSION_LEVEL_UNKNOWN' }
        'reportedSession' { 'SESSION_ID_MISMATCH' }
        'processSession' { 'SESSION_ZERO' }
    }
    foreach ($value in $values) {
        $bad = $base.Clone(); $bad[$field] = $value
        $cases.Add(@{ value = $bad; reason = $reason; name = "reject $field=$value" })
    }
}
foreach ($name in @('LockApp', 'lockapp', 'LogonUI', 'LOGONUI', 'Winlogon', 'winlogon', '', ' ', $null)) {
    $bad = $visible.Clone(); $bad.processName = $name
    $reason = if ([string]::IsNullOrWhiteSpace($name)) { 'FOREGROUND_IDENTITY_UNKNOWN' } else { 'FOREGROUND_SECURE_PROCESS' }
    $cases.Add(@{ value = $bad; reason = $reason; name = "secure or unknown foreground '$name'" })
}
foreach ($sessionId in @(0, 8)) {
    $bad = $visible.Clone(); $bad.foregroundSession = $sessionId
    $cases.Add(@{ value = $bad; reason = 'FOREGROUND_SESSION_MISMATCH'; name = "foreground from session $sessionId" })
}
foreach ($stage in @('current', 'session', 'station', 'desktop', 'foreground')) {
    $bad = $base.Clone(); $bad.fault = $stage
    $reason = switch ($stage) {
        'current' { 'PROCESS_SESSION_QUERY_FAILED' }
        'session' { 'SESSION_INFORMATION_QUERY_FAILED' }
        'station' { 'WINDOW_STATION_QUERY_FAILED' }
        'desktop' { 'INPUT_DESKTOP_QUERY_FAILED' }
        'foreground' { 'FOREGROUND_IDENTITY_QUERY_FAILED' }
    }
    $cases.Add(@{ value = $bad; reason = $reason; name = "query throws at $stage" })
}
foreach ($stage in @('session', 'foreground')) {
    $bad = $base.Clone(); $bad.nullRead = $stage
    $reason = if ($stage -eq 'session') { 'SESSION_INFORMATION_QUERY_FAILED' } else { 'FOREGROUND_IDENTITY_QUERY_FAILED' }
    $cases.Add(@{ value = $bad; reason = $reason; name = "query returns no $stage evidence" })
}

foreach ($case in $cases) {
    $probe = Invoke-SyntheticDesktopProbe $case.value
    $allowed = $case.reason -eq 'INTERACTIVE_DESKTOP_ALLOWED'
    Assert-DesktopTest ($probe.evidence.Allowed -eq $allowed -and $probe.evidence.Reason -ceq $case.reason) `
        "$($case.name): expected $($case.reason), got $($probe.evidence.Reason), allowed=$($probe.evidence.Allowed)"
    if ($case.value.fault) {
        Assert-DesktopTest ($probe.evidence.Win32Error -eq 5 -and $probe.evidence.ErrorType -ceq 'Win32Exception') `
            "$($case.name): preserve the query error category and Win32 code"
        Assert-DesktopTest ($probe.reads[-1] -ceq $case.value.fault) `
            "$($case.name): do not continue reading evidence after a failed query"
    }
    if ($case.value.processSession -eq 0 -or $case.value.state -ne 0 -or $case.value.flags -ne 1) {
        Assert-DesktopTest (-not ($probe.reads -contains 'desktop') -and -not ($probe.reads -contains 'foreground')) `
            "$($case.name): reject service, locked and disconnected sessions before desktop/foreground queries"
    }
}
Assert-DesktopTest ([CortexInteractiveDesktop.Windows]::DesktopEvidenceAllowed(0, 1, 'WinSta0', 'Default', $false, $null)) `
    'the shared classifier must permit zero HWND when the other evidence is complete'
foreach ($case in @(@(7, 1, 7, $true), @(0, 1, 0, $false), @(7, 0, 7, $false), @(7, 2, 7, $false), @(7, 1, 8, $false))) {
    Assert-DesktopTest ([CortexInteractiveDesktop.Windows]::SessionIdentityAllowed($case[0], $case[1], $case[2]) -eq $case[3]) `
        "session identity must bind the caller, version and reported session: $case"
}

# 对真实封送边界做内存回归，避免完整布局/短缓冲区的错误被纯分类测试掩盖。
$nativeType = [CortexInteractiveDesktop.Windows]
$private = [Reflection.BindingFlags]'NonPublic,Static'
$level1 = $nativeType.GetNestedType('SessionInfoLevel1', [Reflection.BindingFlags]::NonPublic)
$infoEx = $nativeType.GetNestedType('SessionInfoEx', [Reflection.BindingFlags]::NonPublic)
$sizeOf = [Runtime.InteropServices.Marshal].GetMethod('SizeOf', [type[]]@([type]))
Assert-DesktopTest ($sizeOf.Invoke($null, [object[]]@($level1)) -eq 224) 'WTSINFOEX_LEVEL1_W must occupy 224 bytes'
Assert-DesktopTest ($sizeOf.Invoke($null, [object[]]@($infoEx)) -eq 232) 'WTSINFOEXW must occupy 232 bytes'
Assert-DesktopTest ([Runtime.InteropServices.Marshal]::OffsetOf($infoEx, 'Data').ToInt32() -eq 8) 'WTSINFOEXW.Data must begin at offset 8'
$parseSession = $nativeType.GetMethod('ReadSessionInformation', $private)
$buffer = [Runtime.InteropServices.Marshal]::AllocHGlobal(232)
try {
    [Runtime.InteropServices.Marshal]::Copy([byte[]]::new(232), 0, $buffer, 232)
    foreach ($write in @(@(0, 1), @(8, 7), @(12, 0), @(16, 1))) {
        [Runtime.InteropServices.Marshal]::WriteInt32($buffer, $write[0], $write[1])
    }
    $parsed = $parseSession.Invoke($null, [object[]]@($buffer, [uint32]232))
    Assert-DesktopTest ($parsed.Level -eq 1 -and $parsed.SessionId -eq 7 -and $parsed.State -eq 0 -and $parsed.Flags -eq 1) `
        'the full native buffer must decode the session fields at their Windows offsets'
    foreach ($length in @(0, 4, 8, 231)) {
        $rejected = $false
        try { $null = $parseSession.Invoke($null, [object[]]@($buffer, [uint32]$length)) }
        catch { $rejected = $_.Exception.GetBaseException() -is [IO.InvalidDataException] }
        Assert-DesktopTest $rejected "a $length-byte WTS buffer must fail closed before dereferencing"
    }
    $rejected = $false
    try { $null = $parseSession.Invoke($null, [object[]]@([IntPtr]::Zero, [uint32]232)) }
    catch { $rejected = $_.Exception.GetBaseException() -is [IO.InvalidDataException] }
    Assert-DesktopTest $rejected 'a null WTS buffer with a plausible length must fail closed'
}
finally { [Runtime.InteropServices.Marshal]::FreeHGlobal($buffer) }

# 只执行实际入口的导入与断言语句；绝不执行 wrapper、child runner 或任何 GUI/driver。
$runnerPath = Join-Path $PSScriptRoot 'run-second-host-acceptance.ps1'
$parseErrors = $null
$runnerAst = [Management.Automation.Language.Parser]::ParseFile($runnerPath, [ref]$null, [ref]$parseErrors)
Assert-DesktopTest (@($parseErrors).Count -eq 0) 'the second-host runner must parse on this PowerShell version'
$moduleImport = $runnerAst.Find({
    param($node)
    $node -is [Management.Automation.Language.CommandAst] -and
        $node.InvocationOperator -eq [Management.Automation.Language.TokenKind]::Dot -and
        $node.Extent.Text -like "*'interactive-desktop.ps1'*"
}, $true)
$desktopGate = $runnerAst.Find({
    param($node)
    $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'Assert-InteractiveDesktop'
}, $true)
$childCalls = $runnerAst.FindAll({
    param($node)
    $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'Invoke-ChildPowerShell'
}, $true)
$oldAssertion = $runnerAst.Find({
    param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Assert-InteractiveDesktop'
}, $true)
Assert-DesktopTest ($null -ne $moduleImport -and $null -ne $desktopGate -and $null -eq $oldAssertion) `
    'the runner must import the standalone desktop gate and must not override it'
Assert-DesktopTest ($moduleImport.Extent.StartOffset -lt $desktopGate.Extent.StartOffset -and $childCalls.Count -gt 0) `
    'the runner must import the module before checking desktop evidence'
foreach ($childCall in $childCalls) {
    Assert-DesktopTest ($desktopGate.Extent.StartOffset -lt $childCall.Extent.StartOffset) `
        'the desktop gate must precede every GUI-capable child invocation'
}
foreach ($scenario in @('allowed', 'query-failed')) {
    $value = $base.Clone()
    if ($scenario -eq 'query-failed') { $value.fault = 'session' }
    $injectedEvidence = (Invoke-SyntheticDesktopProbe $value).evidence
    $assertion = & {
        param($FixtureRoot, $ImportText, $GateText, $Injected)
        $bundle = $FixtureRoot
        . ([scriptblock]::Create($ImportText))
        function Get-InteractiveDesktopEvidence { return $Injected }
        try {
            . ([scriptblock]::Create($GateText))
            return [pscustomobject]@{ rejected = $false; message = '' }
        }
        catch { return [pscustomobject]@{ rejected = $true; message = $_.Exception.Message } }
    } $PSScriptRoot $moduleImport.Extent.Text $desktopGate.Extent.Text $injectedEvidence
    Assert-DesktopTest ($assertion.rejected -eq ($scenario -eq 'query-failed')) `
        "the actual runner import/gate must preserve the $scenario result"
    if ($scenario -eq 'query-failed') {
        Assert-DesktopTest ($assertion.message.Contains('SESSION_INFORMATION_QUERY_FAILED') -and
            $assertion.message.Contains('Win32=5') -and -not $assertion.message.Contains('Windows is locked') -and
            -not $assertion.message.Contains('do-not-echo-query-detail')) `
            'query failure must report insufficient evidence with its code, without a definite lock claim or raw error text'
    }
}
Write-Output "INTERACTIVE_DESKTOP_SELFTEST_PASS checks=$script:desktopChecks scenarios=$($cases.Count) desktop_probes=0 zero_hwnd=allowed"

if ($ProbeCurrentDesktop) {
    # 一次只读原生查询；不要求此刻有前台窗口，也不切换/激活/创建窗口或启动 driver。
    $actual = Get-InteractiveDesktopEvidence
    Assert-DesktopTest (-not [string]::IsNullOrWhiteSpace($actual.Reason)) 'the native probe must provide a result code'
    if ($actual.Allowed) {
        Assert-DesktopTest ($actual.ProcessSessionId -gt 0 -and $actual.SessionLevel -eq 1 -and
            $actual.ProcessSessionId -eq $actual.ReportedSessionId -and $actual.SessionState -eq 0 -and
            $actual.SessionFlags -eq 1 -and $actual.WindowStation -ieq 'WinSta0' -and
            $actual.InputDesktop -ieq 'Default' -and $null -ne $actual.HasForeground -and
            (-not $actual.HasForeground -or $actual.ForegroundIdentityVerified -eq $true) -and
            $null -eq $actual.ErrorType -and $null -eq $actual.Win32Error) `
            'native allow must carry the complete session and input desktop evidence, independently of HWND presence'
    }
    else {
        Assert-DesktopTest ($actual.Reason -cne 'INTERACTIVE_DESKTOP_ALLOWED') 'an unavailable native desktop must remain denied'
    }
    Write-Output ('INTERACTIVE_DESKTOP_NATIVE_READONLY ' + ($actual | ConvertTo-Json -Compress))
}
