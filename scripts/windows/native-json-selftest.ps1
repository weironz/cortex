$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'native-json.ps1')

$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('cortex-native-json-' + [Guid]::NewGuid().ToString('N'))
$null = [IO.Directory]::CreateDirectory($testRoot)
$emitter = Join-Path $testRoot 'emitter.exe'
$originalEncoding = [Console]::OutputEncoding
$checks = 0
try {
    # 输出原始 UTF-8 字节，不经过 PowerShell 的字符串编码，复现真实 Rust CLI。
    Add-Type -OutputAssembly $emitter -OutputType ConsoleApplication -TypeDefinition @'
using System;
using System.Text;
public static class Utf8JsonEmitter {
    public static int Main(string[] args) {
        string value = args[0] == "valid"
            ? "{\"text\":\"\u4e2d\u6587\u5b57\\\\path\\\"end\",\"count\":7}"
            : "PRIVATE_BAD_JSON_MARKER";
        byte[] bytes = new UTF8Encoding(false).GetBytes(value);
        Console.OpenStandardOutput().Write(bytes, 0, bytes.Length);
        return args[0] == "exit" ? 17 : 0;
    }
}
'@
    foreach ($codePage in @(936, 437, 65001)) {
        [Console]::OutputEncoding = [Text.Encoding]::GetEncoding($codePage)
        $result = Invoke-CortexNativeUtf8Json -FilePath $emitter -ArgumentList @('valid')
        if ($result.text -cne ('中文字\path"end') -or $result.count -ne 7) {
            throw "Native JSON changed content under code page $codePage."
        }
        $checks++
        if ([Console]::OutputEncoding.CodePage -ne $codePage) { throw 'Successful call did not restore encoding.' }
        $checks++
        foreach ($mode in @('invalid', 'exit')) {
            $rejected = $false
            try { $null = Invoke-CortexNativeUtf8Json -FilePath $emitter -ArgumentList @($mode) }
            catch {
                $rejected = $true
                if ($_.Exception.Message.Contains('PRIVATE_BAD_JSON_MARKER')) { throw 'Error leaked native JSON.' }
                if ($mode -eq 'exit' -and -not $_.Exception.Message.Contains('17')) { throw 'Native exit code was lost.' }
            }
            if (-not $rejected) { throw "Invalid native result was accepted: $mode." }
            $checks++
            if ([Console]::OutputEncoding.CodePage -ne $codePage) { throw 'Failed call did not restore encoding.' }
            $checks++
        }
    }
    Write-Output "Native UTF-8 JSON self-test passed: $checks checks."
}
finally {
    [Console]::OutputEncoding = $originalEncoding
    # 只移除本次明确创建的两个文件和空目录，不递归清理计算出来的路径。
    if ([IO.File]::Exists($emitter)) { [IO.File]::Delete($emitter) }
    if ([IO.Directory]::Exists($testRoot)) { [IO.Directory]::Delete($testRoot, $false) }
}
