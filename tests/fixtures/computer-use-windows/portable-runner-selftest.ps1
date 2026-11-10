param(
    [Parameter(Mandatory = $true)][string]$BundlePath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$bundle = (Resolve-Path -LiteralPath $BundlePath).ProviderPath
$testExe = Join-Path $bundle 'cortex-local-tests.exe'
$fixtureExe = Join-Path $bundle 'cortex-computer-use-soak-fixture.exe'
$runtimeDll = Join-Path $bundle 'vcruntime140.dll'
$runner = Join-Path $PSScriptRoot 'portable-second-host-smoke.ps1'
$taskRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..\target'))
$testRoot = [IO.Path]::GetFullPath((Join-Path $taskRoot ('cortex-runner-selftest-' + [guid]::NewGuid().ToString('N'))))
if (-not $testRoot.StartsWith($taskRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -or (Test-Path -LiteralPath $testRoot)) {
    throw 'Invalid self-test directory.'
}
$null = New-Item -ItemType Directory -Path $testRoot
$job = $null
$child = $null
$canary = [IntPtr]::Zero
try {
    . $runner -TestExePath $testExe -FixtureExePath $fixtureExe -VCRuntimePath $runtimeDll `
        -CuaDirectory (Join-Path $bundle 'cua') `
        -ExpectedTestSha256 (Get-FileHash -LiteralPath $testExe -Algorithm SHA256).Hash `
        -ExpectedFixtureSha256 (Get-FileHash -LiteralPath $fixtureExe -Algorithm SHA256).Hash `
        -ExpectedVCRuntimeSha256 (Get-FileHash -LiteralPath $runtimeDll -Algorithm SHA256).Hash `
        -OutputPath (Join-Path $testRoot 'unused.json') -ValidateOnly
    Initialize-OwnedProcessSupport
    $childPath = @((Get-PortableSystemPath) -split ';')
    if ($childPath.Count -ne 2 -or $childPath[0] -ne (Join-Path $env:SystemRoot 'System32') -or
        -not [IO.File]::Exists((Join-Path $childPath[1] 'powershell.exe')) -or
        $childPath[1] -ne (Join-Path (Join-Path $env:SystemRoot 'System32') 'WindowsPowerShell\v1.0')) {
        throw 'Isolated PATH must contain only System32 and the system PowerShell needed by owner ACL setup.'
    }
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class CortexInheritCanary {
    [StructLayout(LayoutKind.Sequential)] struct SecurityAttributes {
        public int Length; public IntPtr SecurityDescriptor;
        [MarshalAs(UnmanagedType.Bool)] public bool InheritHandle;
    }
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern IntPtr CreateFile(string name, uint access, uint share, ref SecurityAttributes attr, uint creation, uint flags, IntPtr template);
    [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr handle);
    public static IntPtr Open(string path) {
        SecurityAttributes attr = new SecurityAttributes();
        attr.Length = Marshal.SizeOf(typeof(SecurityAttributes)); attr.InheritHandle = true;
        return CreateFile(path, 0x40000000, 1, ref attr, 1, 0x80, IntPtr.Zero);
    }
}
'@
    $probeSource = @'
using System;
using System.Runtime.InteropServices;
public static class Probe {
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool WriteFile(IntPtr file, byte[] data, uint size, out uint written, IntPtr overlapped);
    public static int Main(string[] args) {
        uint written;
        return WriteFile(new IntPtr(long.Parse(args[0])), new byte[] { 42 }, 1, out written, IntPtr.Zero) ? 1 : 0;
    }
}
'@
    $probeName = 'cortex-probe-' + [guid]::NewGuid().ToString('N')
    $probeExe = Join-Path $testRoot ($probeName + '.exe')
    Add-Type -TypeDefinition $probeSource -OutputAssembly $probeExe -OutputType ConsoleApplication
    $canaryPath = Join-Path $testRoot 'canary.bin'
    $canary = [CortexInheritCanary]::Open($canaryPath)
    if ($canary -eq [IntPtr]::Zero -or $canary -eq [IntPtr]::new(-1)) { throw 'Could not create inheritable test handle.' }
    $taskEnv = New-Object 'System.Collections.Generic.Dictionary[string,string]' ([StringComparer]::OrdinalIgnoreCase)
    $taskEnv['SystemRoot'] = $env:SystemRoot
    $taskEnv['PATH'] = Join-Path $env:SystemRoot 'System32'
    # 关闭 Job 后强制分配失败，验证挂起子进程仍会被回收。
    $brokenJob = New-Object CortexPortableSmoke.OwnedJob
    $brokenJob.Dispose()
    $assignmentFailed = $false
    try { $null = $brokenJob.Start($probeExe, [string[]]@('0'), $testRoot, (Join-Path $testRoot 'failed-start.log'), $true, $taskEnv) }
    catch { $assignmentFailed = $true }
    if (-not $assignmentFailed) { throw 'Expected closed-job assignment to fail.' }
    if (@(Get-Process -Name $probeName -ErrorAction SilentlyContinue).Count -ne 0) { throw 'A failed job assignment left its suspended child running.' }
    $job = New-Object CortexPortableSmoke.OwnedJob
    $child = $job.Start($probeExe, [string[]]@($canary.ToInt64().ToString()), $testRoot, (Join-Path $testRoot 'probe.log'), $true, $taskEnv)
    if (-not $child.Wait(15000) -or $child.ExitCode -ne 0) { throw 'Child inherited the unrelated test handle or probe failed.' }
    $child.Dispose()
    $child = $null
    if (-not [CortexInheritCanary]::CloseHandle($canary)) { throw 'Could not close test handle.' }
    $canary = [IntPtr]::Zero
    if ((Get-Item -LiteralPath $canaryPath).Length -ne 0) { throw 'Child wrote through the unrelated test handle.' }
    $diagnosticPath = Join-Path $testRoot 'diagnostic.log'
    [IO.File]::WriteAllText($diagnosticPath, 'assertion failed; private-example-value; deadline exceeded')
    $diagnostic = Get-SafeLogEvidence -Path $diagnosticPath
    $encoded = $diagnostic | ConvertTo-Json -Compress
    if (-not $diagnostic.panic -or -not $diagnostic.timedOut -or $encoded.Contains('private-example-value')) { throw 'Diagnostic classification leaked content or missed expected categories.' }
    [IO.File]::WriteAllText($diagnosticPath, "thread 'fixture' panicked at C:\private-example-value\computer.rs:5741:9:`n写动作缺少可信宿主主体/调用上下文 private-example-value")
    $diagnostic = Get-SafeLogEvidence -Path $diagnosticPath
    $encoded = $diagnostic | ConvertTo-Json -Compress
    if ($diagnostic.panicFile -cne 'computer.rs' -or $diagnostic.panicLine -ne 5741 -or
        $diagnostic.panicColumn -ne 9 -or -not $diagnostic.missingTrustedHostContext -or $encoded.Contains('private-example-value')) {
        throw 'Panic location classification leaked its path/value or lost numeric location.'
    }
    foreach ($knownFile in @('computer_http_fixture_test.rs', 'computer_portable_fixture_test.rs')) {
        [IO.File]::WriteAllText($diagnosticPath, "panicked at C:\private-example-value\${knownFile}:10:20: private-example-value")
        $diagnostic = Get-SafeLogEvidence -Path $diagnosticPath
        if ($diagnostic.panicFile -cne $knownFile -or $diagnostic.panicLine -ne 10 -or
            ($diagnostic | ConvertTo-Json -Compress).Contains('private-example-value')) {
            throw 'Known fixture source classification must retain only its whitelisted name and numeric location.'
        }
    }
    [IO.File]::WriteAllText($diagnosticPath, '附着 manifest 必须只包含唯一 fixture 和最小动作工具 private-example-value')
    $diagnostic = Get-SafeLogEvidence -Path $diagnosticPath
    if (@($diagnostic.portableContractTags).Count -ne 1 -or $diagnostic.portableContractTags[0] -cne 'attached_manifest' -or
        ($diagnostic | ConvertTo-Json -Compress).Contains('private-example-value')) {
        throw 'Portable contract classification must retain fixed tags only.'
    }
    [IO.File]::WriteAllText($diagnosticPath, 'panicked at C:\private-example-value\unknown.rs:1:2: private-example-value')
    $diagnostic = Get-SafeLogEvidence -Path $diagnosticPath
    if ($null -ne $diagnostic.panicFile -or $null -ne $diagnostic.panicLine -or
        $null -ne $diagnostic.panicColumn -or ($diagnostic|ConvertTo-Json -Compress).Contains('private-example-value')) {
        throw 'Unknown panic source must remain unclassified and redacted.'
    }
    'PASS: failed job assignment cleaned up its child; unrelated handle was withheld; diagnostics contain categories only.'
}
finally {
    if ($null -ne $child) { try { $child.Stop() } finally { $child.Dispose() } }
    if ($null -ne $job) { $job.Dispose() }
    if ($canary -ne [IntPtr]::Zero -and $canary -ne [IntPtr]::new(-1)) { $null = [CortexInheritCanary]::CloseHandle($canary) }
    if (Test-Path -LiteralPath $testRoot -PathType Container) { [IO.Directory]::Delete($testRoot, $true) }
}
