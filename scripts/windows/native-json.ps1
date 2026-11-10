function Invoke-CortexNativeUtf8Json {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$ArgumentList = @()
    )

    # Rust 输出 UTF-8；PS 5.1 却按控制台代码页解码。中文与转义符组合会
    # 连 JSON 结构也损坏，不能靠解析失败后改用不完整的依赖清单蒙混过去。
    $previousEncoding = [Console]::OutputEncoding
    try {
        [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
        $raw = & $FilePath @ArgumentList | Out-String
        $nativeExitCode = $LASTEXITCODE
        if ($nativeExitCode -ne 0) {
            throw "Native JSON command failed (exit $nativeExitCode)."
        }
        try {
            return (ConvertFrom-Json -InputObject $raw -ErrorAction Stop)
        }
        catch {
            # ConvertFrom-Json 默认会把整个输入放入异常；依赖清单很大，
            # 这里保留分类，不把任意原始工具输出倾倒到构建日志。
            throw 'Native command did not return valid UTF-8 JSON.'
        }
    }
    finally {
        [Console]::OutputEncoding = $previousEncoding
    }
}
