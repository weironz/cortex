<#
.SYNOPSIS
  只操作独立复制的专用fixture，验证真实Cua的AX-only链路。
.DESCRIPTION
  使用预先编译的精确ignored测试；不读取用户配置或调用真实模型。
  结果只证明该二进制和驱动的fixture链路，不替代产品审批/UI/生产验收。
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$TestExePath,
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedTestSha256,
    [Parameter(Mandatory = $true)][string]$FixtureExePath,
    [Parameter(Mandatory = $true)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedFixtureSha256,
    [Parameter(Mandatory = $true)][string]$DriverPath,
    [Parameter(Mandatory = $true)][string]$OutputPath
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$testName = 'computer::real::tests::cua_ax_only_fixture_sets_value_clicks_and_verifies_without_images'
$driverSha = 'ffa58d88779ee219304b0c307f4cfa970fe822f830c6ecf9254eda5c29e22bf2'
function Assert-Hash([string]$Path, [string]$Expected) {
    if ((Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $Expected.ToLowerInvariant()) {
        throw '输入二进制摘要不匹配，没有启动测试。'
    }
}
Assert-Hash $TestExePath $ExpectedTestSha256
Assert-Hash $FixtureExePath $ExpectedFixtureSha256
Assert-Hash $DriverPath $driverSha
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('cortex-ax-smoke-' + [guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($temporary)
$testCopy = Join-Path $temporary 'cortex-local-tests.exe'
$fixtureCopy = Join-Path $temporary 'cortex-computer-use-windows-fixture.exe'
[IO.File]::Copy([IO.Path]::GetFullPath($TestExePath), $testCopy, $false)
[IO.File]::Copy([IO.Path]::GetFullPath($FixtureExePath), $fixtureCopy, $false)
Assert-Hash $testCopy $ExpectedTestSha256
Assert-Hash $fixtureCopy $ExpectedFixtureSha256
$listed = & $testCopy --list 2>&1 | Out-String
if ($LASTEXITCODE -ne 0 -or -not $listed.Contains($testName + ': test')) { throw '缺少精确的AX-only测试，不能把0 tests计作通过。' }
$socket = '\\.\pipe\cortex-ax-smoke-' + [guid]::NewGuid().ToString('N')
$manifest = Join-Path $temporary 'manifest.json'
@{
    version = 3; expires_after = '30m'; idle_timeout = '5m'
    resources = @{apps = @(@{executable = $fixtureCopy; windows = 'all'; terminate = 'deny'})}
    allow = @{tools = @('start_session','get_window_state','click','type_text','set_value','press_key','scroll','end_session')}
} | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $manifest -Encoding UTF8
$env:CUA_DRIVER_RS_HOME = Join-Path $temporary 'driver-home'
$env:CUA_DRIVER_RS_TELEMETRY_ENABLED = 'false'
$env:CUA_DRIVER_RS_UPDATE_CHECK = 'false'
$env:CUA_TELEMETRY_ENABLED = 'false'
$env:CUA_TELEMETRY = '0'
$env:CUA_TELEMETRY_FORBID_NETWORK = '1'
$env:DO_NOT_TRACK = '1'
$env:LOCALAPPDATA = Join-Path $temporary 'local-appdata'
$env:CORTEX_LOCAL_STATE_DIR = Join-Path $temporary 'state'
$env:CORTEX_TOKEN = ''
[void][IO.Directory]::CreateDirectory($env:CUA_DRIVER_RS_HOME)
[void][IO.Directory]::CreateDirectory($env:LOCALAPPDATA)
$daemon = $null; $fixture = $null; $test = $null
$started = [DateTimeOffset]::UtcNow
$result = [ordered]@{schemaVersion=1;scope='real_driver_and_owned_fixture_ax_only';status='running';testName=$testName;testSha256=$ExpectedTestSha256;fixtureSha256=$ExpectedFixtureSha256;driverSha256=$driverSha;startedAt=$started.ToString('o');workDirectory=$temporary;testExitCode=$null;testSummaryMatched=$false;failure=$null;cleanupErrors=@();finishedAt=$null;durationSeconds=$null}
try {
    $daemon = Start-Process -FilePath $DriverPath -ArgumentList @('serve','--permission-mode','bounded','--capability-manifest', ('"'+$manifest+'"'),'--approve-capability-manifest','--socket',$socket,'--no-overlay') -PassThru -WindowStyle Hidden -RedirectStandardOutput (Join-Path $temporary 'daemon.stdout.log') -RedirectStandardError (Join-Path $temporary 'daemon.stderr.log')
    $ready = $false
    $budget = [Diagnostics.Stopwatch]::StartNew()
    while ($budget.Elapsed.TotalSeconds -lt 30) {
        $daemon.Refresh()
        if ($daemon.HasExited) { throw '专用Cua daemon提前退出。' }
        $previous = $ErrorActionPreference
        try { $ErrorActionPreference = 'Continue'; $status = & $DriverPath status --socket $socket 2>&1 | Out-String; $statusExit=$LASTEXITCODE }
        finally { $ErrorActionPreference = $previous }
        if ($statusExit -eq 0 -and $status -match 'permission mode: bounded' -and $status -match 'approved_at_startup=true' -and $status -match 'valid=true') { $ready = $true; break }
        Start-Sleep -Milliseconds 100
    }
    if (-not $ready) { throw '专用bounded daemon未就绪。' }
    # 这是用户已授权的可见测试工具，只有独立复制的程序能进入本次allowlist。
    $fixture = Start-Process -FilePath $fixtureCopy -ArgumentList @('--no-secure-field') -PassThru
    Start-Sleep -Milliseconds 800
    $fixture.Refresh()
    if ($fixture.HasExited) { throw '专用fixture提前退出。' }
    $env:CORTEX_CUA_DRIVER_PATH = [IO.Path]::GetFullPath($DriverPath)
    $env:CORTEX_CUA_DRIVER_SOCKET = $socket
    $env:CORTEX_CUA_EXECUTABLE_ALLOWLIST = $fixtureCopy
    $env:CORTEX_CUA_PACKAGED_APP_ALLOWLIST = ''
    $env:CORTEX_CUA_ALLOW_ACTIONS = '1'
    $env:CORTEX_CUA_MANIFEST_PATH = $manifest
    $env:CORTEX_CUA_DRIVER_SHA256 = $driverSha
    $env:CORTEX_CUA_SESSION = 'ax-smoke-' + [guid]::NewGuid().ToString('N')
    $testLog = Join-Path $temporary 'test.stdout.log'
    $test = Start-Process -FilePath $testCopy -ArgumentList @('--ignored','--exact',$testName,'--nocapture','--test-threads=1') -PassThru -WindowStyle Hidden -RedirectStandardOutput $testLog -RedirectStandardError (Join-Path $temporary 'test.stderr.log')
    # PowerShell 5.1 的 PassThru 对象须先持有句柄，否则退出后可能读到空 ExitCode。
    $null = $test.Handle
    if (-not $test.WaitForExit(180000)) { throw 'AX-only fixture测试超过180秒。' }
    $test.WaitForExit()
    $result.testExitCode = $test.ExitCode
    $body = [IO.File]::ReadAllText($testLog)
    $result.testSummaryMatched = $body -match 'test result: ok\. 1 passed; 0 failed'
    if ($null -eq $result.testExitCode) { throw '测试已退出但未取得退出码，不能判定通过。' }
    if ($result.testExitCode -ne 0 -or -not $result.testSummaryMatched) { throw '真实AX-only fixture断言失败，详见保留的专用日志。' }
    $result.status='passed'
}
catch { $result.status='failed'; $result.failure=$_.Exception.Message }
finally {
    # 只持有并结束本脚本创建的进程对象；不按名称枚举或停止用户程序。
    foreach ($owned in @($test,$fixture,$daemon)) {
        if ($null -ne $owned) {
            try { $owned.Refresh(); if (-not $owned.HasExited) { $owned.Kill(); if (-not $owned.WaitForExit(5000)) { throw '结束超时' } } }
            catch { $result.cleanupErrors += '专用进程清理失败'; $result.status='failed' }
            finally { $owned.Dispose() }
        }
    }
    $result.finishedAt=[DateTimeOffset]::UtcNow.ToString('o')
    $result.durationSeconds=[Math]::Round(([DateTimeOffset]::UtcNow-$started).TotalSeconds,3)
    $absoluteOutput=[IO.Path]::GetFullPath($OutputPath)
    [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($absoluteOutput))
    $result|ConvertTo-Json -Depth 5|Set-Content -LiteralPath $absoluteOutput -Encoding UTF8
}
if ($result.status -ne 'passed') { throw $result.failure }
$result|ConvertTo-Json -Depth 5
