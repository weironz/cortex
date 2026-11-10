<#
.SYNOPSIS
用当前源码编译固定 Windows x64 release bin test，并生成本机构建来源记录。
.EXAMPLE
$env:CARGO_TARGET_DIR = 'C:\Users\willz\AppData\Local\Temp\cortex-input-recovery-nvme-20261110'
$env:CARGO_BUILD_JOBS = '2'
.\build-second-host-test-artifact.ps1 -ProofDirectory "$env:TEMP\cortex-test-proof-<新名称>"
.DESCRIPTION
内部只执行 cargo metadata 和 cargo test --no-run；从不运行 exe 或 GUI。
proof 只关联本次 Cargo 输出、稳定输入清单与二进制 hash；不提供签名或跨主机验证。
输入范围与限制见 test-artifact-provenance.md。
#>
param(
    [Parameter(Mandatory = $true)][string]$ProofDirectory
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. ([IO.Path]::Combine($PSScriptRoot, 'test-artifact-provenance.ps1'))
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
New-CortexTestArtifactProof -RepositoryRoot $root -ProofDirectory $ProofDirectory
