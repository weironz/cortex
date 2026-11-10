#requires -Version 5.1
[CmdletBinding()]
param([string]$CuaSourceRoot='D:\codes\_ref\cua')
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'actions-lib.ps1')
$shared=@('cua-readonly-ab.ps1','cua-readonly-steady.ps1','owned-job.cs')
$before=@{}
foreach($name in $shared){$path=Join-Path $PSScriptRoot ('..\readonly-ab\'+$name);$before[$path]=Get-CAFileSha256 $path}
foreach($path in [IO.Directory]::GetFiles($PSScriptRoot,'*.ps1')){
    $bytes=[IO.File]::ReadAllBytes($path)
    if($bytes.Length -lt 3 -or $bytes[0] -ne 239 -or $bytes[1] -ne 187 -or $bytes[2] -ne 191){throw 'CA_SOURCE_BOM'}
    $tokens=$null;$errors=$null
    $ast=[Management.Automation.Language.Parser]::ParseFile($path,[ref]$tokens,[ref]$errors)
    if($errors.Count -ne 0){throw 'CA_SOURCE_PARSE'}
    $forbidden=@($ast.FindAll({param($node) $node -is [Management.Automation.Language.CommandAst] -and
        $node.GetCommandName() -in @('Start-Process','Stop-Process','Get-Process','Invoke-Expression','Set-Clipboard','Get-Clipboard','Get-FileHash')},$true))
    if($forbidden.Count -ne 0){throw 'CA_SOURCE_FORBIDDEN_COMMAND'}
}
. (Join-Path $PSScriptRoot 'headless-selftest.ps1')
Invoke-CAHeadlessSelfTests
Initialize-CANative
Add-Type -Path (Join-Path $PSScriptRoot '..\readonly-ab\owned-job.cs')
$provenance=[IO.File]::ReadAllText((Join-Path $PSScriptRoot 'schema-source.json'),[Text.Encoding]::UTF8)|ConvertFrom-Json
Require-CA ($provenance.base_commit -ceq '85e2b1318bd77ccf3cfa88ccdf875a4bb7eaed19') 'CA_SOURCE_BASE_COMMIT'
foreach($item in $provenance.files){
    $actual=Get-CAFileSha256 (Join-Path $CuaSourceRoot $item.path)
    Require-CA ($actual -ceq $item.sha256) 'CA_SOURCE_SCHEMA_DRIFT'
}
Require-CA ((Get-CAFileSha256 (Join-Path $PSScriptRoot '..\src\Program.cs')) -ceq
    $provenance.fixture_source_sha256) 'CA_SOURCE_FIXTURE_DRIFT'
foreach($entry in $before.GetEnumerator()){
    Require-CA ((Get-CAFileSha256 $entry.Key) -ceq $entry.Value) 'CA_SHARED_SOURCE_CHANGED'
}
Write-Output 'CA_SOURCE_VALIDATION_PASS syntax=PS51_BOM schemas=frozen_0.38 native=compile_only jobs_started=0 shared_sources=unchanged gui_driver_started=0'
