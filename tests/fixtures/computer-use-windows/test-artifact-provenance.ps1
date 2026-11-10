# 打包器校验与构建入口共用同一套规则；仅加载系统模块并定义函数，不运行测试 exe。
# Git Bash 传来的 PSModulePath 可能指向 PowerShell 7；PS 5.1 不能靠自动发现
# 找到 Get-FileHash 等命令。所有入口先按当前宿主的系统路径加载，再共用下列函数。
# schema 1 固定使用 PS5.1 的排序/JSON 口径，先拒绝不兼容宿主，不能误报源码漂移。
if ($PSVersionTable.PSVersion.Major -ne 5 -or $PSVersionTable.PSVersion.Minor -ne 1) {
    throw '来源记录入口仅支持 Windows PowerShell 5.1；请用 powershell.exe -NoProfile -File 执行，不能用 pwsh 或直接在 PS7 中加载。'
}
foreach ($cortexArtifactModule in @(
    'Microsoft.PowerShell.Utility', 'Microsoft.PowerShell.Management', 'Microsoft.PowerShell.Security'
)) {
    $cortexArtifactModulePath = [IO.Path]::Combine($PSHOME, 'Modules', $cortexArtifactModule, "$cortexArtifactModule.psd1")
    Import-Module -Name $cortexArtifactModulePath -ErrorAction Stop
}
$script:CortexArtifactTriple = 'x86_64-pc-windows-msvc'

function Get-CortexArtifactSha256 {
    param([string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-CortexArtifactDigest {
    param($Value)
    $bytes = [Text.Encoding]::UTF8.GetBytes((ConvertTo-Json -InputObject $Value -Depth 40 -Compress))
    $sha = [Security.Cryptography.SHA256]::Create()
    try { return ([BitConverter]::ToString($sha.ComputeHash($bytes))).Replace('-', '').ToLowerInvariant() }
    finally { $sha.Dispose() }
}

function Assert-CortexArtifactNoReparse {
    param([string]$Path)
    $cursor = [IO.Path]::GetFullPath($Path)
    while ($cursor) {
        if (Test-Path -LiteralPath $cursor) {
            if ((Get-Item -LiteralPath $cursor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "拒绝经过 reparse point 的路径：$Path"
            }
        }
        $cursor = [IO.Path]::GetDirectoryName($cursor)
    }
}

function Get-CortexArtifactBuildArguments {
    return @('test', '--release', '-p', 'cortex-local', '--bin', 'cortex-local', '--no-run',
        '--target', $script:CortexArtifactTriple, '--locked', '--message-format=json')
}

function Get-CortexArtifactContext {
    param([string]$RepositoryRoot)
    $root = [IO.Path]::GetFullPath($RepositoryRoot)
    . (Join-Path $root 'scripts\windows\native-json.ps1')
    Push-Location -LiteralPath $root
    try {
        $metadata = Invoke-CortexNativeUtf8Json -FilePath cargo -ArgumentList @(
            'metadata', '--format-version', '1', '--locked', '--offline',
            '--filter-platform', $script:CortexArtifactTriple
        )
    }
    finally { Pop-Location }
    if ([IO.Path]::GetFullPath($metadata.workspace_root) -ne $root) {
        throw 'Cargo metadata 的 workspace_root 与指定源码目录不同。'
    }
    $manifest = Join-Path $root 'crates\cortex-local\Cargo.toml'
    $packages = @($metadata.packages | Where-Object {
        $_.name -ceq 'cortex-local' -and [IO.Path]::GetFullPath($_.manifest_path) -eq $manifest
    })
    if ($packages.Count -ne 1) { throw 'Cargo metadata 必须唯一标识当前 cortex-local 包。' }
    $package = $packages[0]
    $targets = @($package.targets | Where-Object {
        $_.name -ceq 'cortex-local' -and @($_.kind) -ccontains 'bin' -and
        [IO.Path]::GetFullPath($_.src_path) -eq (Join-Path $root 'crates\cortex-local\src\main.rs')
    })
    if ($targets.Count -ne 1) { throw '当前源码必须提供 cortex-local 的 src/main.rs bin target。' }
    $toml = [IO.File]::ReadAllText((Join-Path $root 'Cargo.toml'))
    $section = [regex]::Match($toml, '(?ms)^\[workspace\.package\]\s*\r?\n(?<body>.*?)(?=^\[|\z)')
    $version = [regex]::Match($section.Groups['body'].Value, '(?m)^version\s*=\s*"([^"]+)"\s*(?:#.*)?$')
    if (-not $version.Success -or $version.Groups[1].Value -cne $package.version) {
        throw 'workspace.package.version 与当前 cortex-local 版本不一致或无法读取。'
    }
    $roots = @($metadata.packages | Where-Object { $null -eq $_.source } | ForEach-Object {
        [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($_.manifest_path))
    } | Sort-Object -Unique)
    if ($roots -contains $root) { throw '此 helper 仅支持当前 Cortex 的虚拟 workspace 布局。' }
    return [ordered]@{
        repositoryRoot = $root
        version = $package.version
        packageId = $package.id
        manifestPath = $manifest
        sourcePath = [IO.Path]::GetFullPath($targets[0].src_path)
        sourceRoots = $roots
        targetDirectory = [IO.Path]::GetFullPath($metadata.target_directory)
    }
}

function Get-CortexArtifactSourceIdentity {
    param($Context)
    # 打包时允许 CARGO_TARGET_DIR 不同；实际 exe 仍只取 proof 绑定的那一个。
    return [ordered]@{
        repositoryRoot = $Context.repositoryRoot
        version = $Context.version
        packageId = $Context.packageId
        manifestPath = $Context.manifestPath
        sourcePath = $Context.sourcePath
        sourceRoots = @($Context.sourceRoots)
    }
}

function Get-CortexArtifactInputs {
    param($Context)
    $files = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
    $root = $Context.repositoryRoot
    foreach ($name in @('Cargo.toml', 'Cargo.lock')) {
        $path = Join-Path $root $name
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "缺少 Rust/Cargo 输入：$path" }
        $null = $files.Add($path)
    }
    foreach ($name in @('rust-toolchain', 'rust-toolchain.toml')) {
        $path = Join-Path $root $name
        if (Test-Path -LiteralPath $path -PathType Leaf) { $null = $files.Add($path) }
    }
    # Cargo 从工作目录逐级读取配置；也记录用户配置的 hash，不读取 credentials。
    $cursor = $root
    while ($cursor) {
        foreach ($name in @('.cargo\config', '.cargo\config.toml')) {
            $path = Join-Path $cursor $name
            if (Test-Path -LiteralPath $path -PathType Leaf) { $null = $files.Add($path) }
        }
        $cursor = [IO.Path]::GetDirectoryName($cursor)
    }
    $cargoHomePath = $env:CARGO_HOME
    if ([string]::IsNullOrWhiteSpace($cargoHomePath)) { $cargoHomePath = Join-Path $env:USERPROFILE '.cargo' }
    foreach ($name in @('config', 'config.toml')) {
        $path = [IO.Path]::GetFullPath((Join-Path $cargoHomePath $name))
        if (Test-Path -LiteralPath $path -PathType Leaf) { $null = $files.Add($path) }
    }
    $directories = @($Context.sourceRoots)
    foreach ($name in @('migrations', 'migrations-global')) {
        $path = Join-Path $root $name
        if (Test-Path -LiteralPath $path -PathType Container) { $directories += $path }
    }
    $pending = New-Object 'System.Collections.Generic.Stack[string]'
    foreach ($directory in $directories) { $pending.Push($directory) }
    while ($pending.Count -gt 0) {
        $directory = $pending.Pop()
        Assert-CortexArtifactNoReparse $directory
        foreach ($item in Get-ChildItem -LiteralPath $directory -Force) {
            # 包内资源也会被 include_bytes!/build.rs 消费，不能只枚举 .rs。
            if ($item.PSIsContainer -and ($item.Name -in @('.git', 'target') -or
                $item.FullName -eq $Context.targetDirectory)) { continue }
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "源码输入不支持 reparse point：$($item.FullName)"
            }
            if ($item.PSIsContainer) { $pending.Push($item.FullName) }
            else { $null = $files.Add($item.FullName) }
        }
    }
    $entries = @($files | Sort-Object | ForEach-Object {
        Assert-CortexArtifactNoReparse $_
        [ordered]@{ path = $_; sha256 = Get-CortexArtifactSha256 $_ }
    })
    return [ordered]@{ files = $entries; sha256 = Get-CortexArtifactDigest $entries }
}

function Get-CortexArtifactBuildEnvironmentHash {
    # 只记录编译开关的摘要，不把令牌或完整进程环境写进 proof。
    $entries = @(Get-ChildItem Env: | Where-Object {
        $_.Name -match '^(RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|RUSTC|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|RUSTUP_TOOLCHAIN|CARGO_HOME|CARGO_TARGET_DIR|CARGO_BUILD_.*|CARGO_PROFILE_RELEASE_.*|CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_.*|CC|CXX|AR|CFLAGS|CXXFLAGS|LDFLAGS)$'
    } | Sort-Object Name | ForEach-Object { [ordered]@{ name = $_.Name; value = $_.Value } })
    return Get-CortexArtifactDigest $entries
}

function Read-CortexTestCompilerArtifact {
    param([string]$NdjsonPath, $Context)
    $matches = New-Object 'System.Collections.Generic.List[object]'
    $finished = $false
    foreach ($line in [IO.File]::ReadLines($NdjsonPath, [Text.UTF8Encoding]::new($false, $true))) {
        # Cargo 的 stdout 是 NDJSON，且过程宏可能夹杂普通文本；不能整段 ConvertFrom-Json。
        if (-not $line.TrimStart().StartsWith('{')) { continue }
        try { $message = ConvertFrom-Json -InputObject $line -ErrorAction Stop }
        catch { throw 'Cargo NDJSON 含损坏或截断的 JSON 行。' }
        if ($null -eq $message.PSObject.Properties['reason']) { continue }
        if ($finished) { throw 'Cargo build-finished 之后出现额外消息。' }
        if ($message.reason -ceq 'build-finished') {
            if ($message.success -isnot [bool] -or -not $message.success) { throw 'Cargo 未成功完成构建。' }
            $finished = $true
            continue
        }
        if ($message.reason -cne 'compiler-artifact' -or $message.package_id -cne $Context.packageId) { continue }
        if ($message.target.name -cne 'cortex-local' -or @($message.target.kind) -cnotcontains 'bin' -or
            $message.profile.test -isnot [bool] -or -not $message.profile.test) { continue }
        if ([IO.Path]::GetFullPath($message.manifest_path) -ne $Context.manifestPath -or
            [IO.Path]::GetFullPath($message.target.src_path) -ne $Context.sourcePath) {
            throw 'compiler-artifact 的 manifest/source 与当前 cortex-local 不一致。'
        }
        if ([string]::IsNullOrWhiteSpace($message.executable) -or -not [IO.Path]::IsPathRooted($message.executable)) {
            throw 'compiler-artifact 未提供绝对 test executable 路径。'
        }
        $exe = [IO.Path]::GetFullPath($message.executable)
        $expectedParent = Join-Path $Context.targetDirectory ($script:CortexArtifactTriple + '\release\deps')
        if ([IO.Path]::GetDirectoryName($exe) -ne $expectedParent -or
            [IO.Path]::GetExtension($exe) -ine '.exe' -or @($message.filenames) -notcontains $exe) {
            throw 'test executable 必须来自 Cargo 实际 target_directory 下的固定 Windows x64 release/deps。'
        }
        if ($message.fresh -isnot [bool]) { throw 'compiler-artifact 缺少 Cargo fresh 状态。' }
        $matches.Add($message)
    }
    if (-not $finished -or $matches.Count -ne 1) {
        throw '必须恰有一个 cortex-local bin test compiler-artifact 和成功的 build-finished。'
    }
    return $matches[0]
}

function Invoke-CortexArtifactCargoBuild {
    param([string]$RepositoryRoot, [string]$NdjsonPath)
    $encoding = [Console]::OutputEncoding
    $writer = $null
    Push-Location -LiteralPath $RepositoryRoot
    try {
        [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
        $writer = [IO.StreamWriter]::new($NdjsonPath, $false, [Text.UTF8Encoding]::new($false))
        $arguments = @(Get-CortexArtifactBuildArguments)
        & cargo @arguments | ForEach-Object { $writer.WriteLine([string]$_) }
        if ($LASTEXITCODE -ne 0) { throw "Cargo --no-run 构建失败，exit=$LASTEXITCODE；不生成 proof。" }
    }
    finally {
        if ($null -ne $writer) { $writer.Dispose() }
        [Console]::OutputEncoding = $encoding
        Pop-Location
    }
}

function Assert-CortexTestArtifactProof {
    param([string]$RepositoryRoot, [string]$TestExePath, [string]$ProofPath)
    if ([string]::IsNullOrWhiteSpace($TestExePath) -or [string]::IsNullOrWhiteSpace($ProofPath)) {
        throw '必须显式传入 -TestExePath 与 -TestArtifactProofPath；先运行 build-second-host-test-artifact.ps1 -ProofDirectory <全新目录>。禁止猜测旧 exe。'
    }
    Assert-CortexArtifactNoReparse $ProofPath
    Assert-CortexArtifactNoReparse $TestExePath
    $proof = ConvertFrom-Json -InputObject ([IO.File]::ReadAllText($ProofPath, [Text.Encoding]::UTF8))
    if ($proof.schemaVersion -ne 1 -or $proof.purpose -cne 'local-cargo-test-artifact-provenance' -or
        $proof.target -cne $script:CortexArtifactTriple -or $proof.profile -cne 'release' -or
        $proof.cargoExitCode -ne 0 -or
        (Get-CortexArtifactDigest @($proof.cargoArguments)) -cne (Get-CortexArtifactDigest @(Get-CortexArtifactBuildArguments))) {
        throw 'proof 的版本、用途、target、profile 或 Cargo 命令不符合固定构建契约。'
    }
    $context = Get-CortexArtifactContext $RepositoryRoot
    if ((Get-CortexArtifactDigest (Get-CortexArtifactSourceIdentity $context)) -cne
        (Get-CortexArtifactDigest (Get-CortexArtifactSourceIdentity $proof.context))) {
        throw 'proof 的源码目录、包版本或 source target 与当前源码不同。'
    }
    foreach ($snapshot in @($proof.inputsBefore, $proof.inputsAfter)) {
        if ($snapshot.sha256 -cne (Get-CortexArtifactDigest @($snapshot.files))) { throw 'proof 输入清单 hash 不匹配。' }
    }
    $current = Get-CortexArtifactInputs $context
    if ($proof.inputsBefore.sha256 -cne $proof.inputsAfter.sha256 -or
        $current.sha256 -cne $proof.inputsAfter.sha256 -or
        $proof.buildEnvironmentBefore -cne $proof.buildEnvironmentAfter) {
        throw '编译期间或编译之后 Rust/Cargo 输入发生变化；请重新用 helper 构建。'
    }
    $ndjson = Join-Path ([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($ProofPath))) 'cargo-messages.ndjson'
    Assert-CortexArtifactNoReparse $ndjson
    if ((Get-CortexArtifactSha256 $ndjson) -cne $proof.ndjsonSha256) { throw 'Cargo NDJSON hash 不匹配。' }
    $artifact = Read-CortexTestCompilerArtifact $ndjson $proof.context
    if ((Get-CortexArtifactDigest $artifact) -cne (Get-CortexArtifactDigest $proof.artifact) -or
        [IO.Path]::GetFullPath($TestExePath) -ne [IO.Path]::GetFullPath($artifact.executable) -or
        (Get-CortexArtifactSha256 $TestExePath) -cne $proof.testExeSha256) {
        throw '显式 exe、proof、compiler-artifact 或 executable SHA256 不匹配。'
    }
    return $proof
}

function New-CortexTestArtifactProof {
    param([string]$RepositoryRoot, [string]$ProofDirectory)
    $root = [IO.Path]::GetFullPath($RepositoryRoot)
    $directory = [IO.Path]::GetFullPath($ProofDirectory)
    Assert-CortexArtifactNoReparse $directory
    $tempPrefix = [IO.Path]::GetTempPath().TrimEnd('\') + '\'
    $distPrefix = (Join-Path $root 'dist') + '\'
    if ((-not $directory.StartsWith($tempPrefix, [StringComparison]::OrdinalIgnoreCase) -and
        -not $directory.StartsWith($distPrefix, [StringComparison]::OrdinalIgnoreCase)) -or
        ($directory.StartsWith($root + '\', [StringComparison]::OrdinalIgnoreCase) -and
        -not $directory.StartsWith($distPrefix, [StringComparison]::OrdinalIgnoreCase))) {
        throw 'ProofDirectory 必须位于系统 Temp 或当前 workspace/dist 内，不能写入源码目录。'
    }
    # 不清空任何旧目录；proof 与日志必须来自这次独立调用。
    if (Test-Path -LiteralPath $directory) { throw 'ProofDirectory 必须是全新目录；不会覆盖旧 proof。' }
    $context = Get-CortexArtifactContext $root
    foreach ($sourceRoot in @($context.sourceRoots) + @($root)) {
        if ($directory -eq $sourceRoot -or $sourceRoot.StartsWith($directory + '\', [StringComparison]::OrdinalIgnoreCase) -or
            ($sourceRoot -ne $root -and $directory.StartsWith($sourceRoot + '\', [StringComparison]::OrdinalIgnoreCase))) {
            throw 'ProofDirectory 不得覆盖或写入源码包目录。'
        }
    }
    $before = Get-CortexArtifactInputs $context
    $environmentBefore = Get-CortexArtifactBuildEnvironmentHash
    $started = [DateTimeOffset]::UtcNow.ToString('o')
    $null = New-Item -ItemType Directory -Path $directory
    $ndjson = Join-Path $directory 'cargo-messages.ndjson'
    [IO.File]::WriteAllText((Join-Path $directory 'inputs-before.json'),
        (ConvertTo-Json -InputObject $before -Depth 10) + "`n", [Text.UTF8Encoding]::new($false))
    try { Invoke-CortexArtifactCargoBuild $root $ndjson }
    finally {
        # 编译失败也保留前后输入记录；绝不把失败日志与旧 proof 混在一起。
        $afterContext = Get-CortexArtifactContext $root
        $after = Get-CortexArtifactInputs $afterContext
        $environmentAfter = Get-CortexArtifactBuildEnvironmentHash
        [IO.File]::WriteAllText((Join-Path $directory 'inputs-after.json'),
            (ConvertTo-Json -InputObject $after -Depth 10) + "`n", [Text.UTF8Encoding]::new($false))
    }
    if ((Get-CortexArtifactDigest $context) -cne (Get-CortexArtifactDigest $afterContext) -or
        $before.sha256 -cne $after.sha256 -or $environmentBefore -cne $environmentAfter) {
        throw '编译前后 Rust/Cargo 输入或配置变化；保留日志，不生成 proof。'
    }
    $artifact = Read-CortexTestCompilerArtifact $ndjson $context
    Assert-CortexArtifactNoReparse $artifact.executable
    $proof = [ordered]@{
        schemaVersion = 1
        purpose = 'local-cargo-test-artifact-provenance'
        target = $script:CortexArtifactTriple
        profile = 'release'
        cargoArguments = @(Get-CortexArtifactBuildArguments)
        cargoExitCode = 0
        startedAt = $started
        finishedAt = [DateTimeOffset]::UtcNow.ToString('o')
        context = $context
        inputsBefore = $before
        inputsAfter = $after
        buildEnvironmentBefore = $environmentBefore
        buildEnvironmentAfter = $environmentAfter
        artifact = $artifact
        testExeSha256 = Get-CortexArtifactSha256 $artifact.executable
        ndjsonSha256 = Get-CortexArtifactSha256 $ndjson
    }
    $proofPath = Join-Path $directory 'test-artifact-proof.json'
    [IO.File]::WriteAllText($proofPath, (ConvertTo-Json -InputObject $proof -Depth 40) + "`n", [Text.UTF8Encoding]::new($false))
    return [pscustomobject]@{ testExePath = $artifact.executable; testArtifactProofPath = $proofPath; cargoFresh = $artifact.fresh }
}

function Assert-CortexBundleOutput {
    param([string]$RepositoryRoot, [string]$OutputDirectory, [string[]]$InputPaths)
    $root = [IO.Path]::GetFullPath($RepositoryRoot)
    $output = [IO.Path]::GetFullPath($OutputDirectory)
    $dist = Join-Path $root 'dist'
    $name = [IO.Path]::GetFileName($output)
    if ([IO.Path]::GetDirectoryName($output) -ne $dist -or $name -notmatch '^[a-zA-Z0-9][a-zA-Z0-9._-]*$' -or
        $name.EndsWith('.') -or $name -match '^(?i:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)') {
        throw 'OutputDirectory 必须是当前 workspace/dist 下的直接子目录，不能是根目录、源目录或外部目录。'
    }
    Assert-CortexArtifactNoReparse $output
    foreach ($inputPath in $InputPaths) {
        $inputFullPath = [IO.Path]::GetFullPath($inputPath)
        if ($inputFullPath -eq $output -or $inputFullPath.StartsWith($output + '\', [StringComparison]::OrdinalIgnoreCase)) {
            throw '输出目录包含打包源输入，拒绝覆盖。'
        }
    }
    foreach ($path in @($output, "$output.zip", "$output.zip.sha256")) {
        if (Test-Path -LiteralPath $path) { throw "输出已存在；-Force 不再递归覆盖，请选全新目录：$path" }
    }
    return $output
}
