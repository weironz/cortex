function Get-CortexSbomCargoSourceProperties {
    param(
        [Parameter(Mandatory = $true)]$Package,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][string[]]$WorkspaceMemberIds,
        [Parameter(Mandatory = $true)][string]$SourceRoot
    )
    $declared = [string]$Package.source
    if (-not [string]::IsNullOrWhiteSpace($declared)) {
        return ,@{ name = 'cortex:source'; value = $declared }
    }
    if ($WorkspaceMemberIds -ccontains [string]$Package.id) {
        return ,@{ name = 'cortex:source'; value = 'workspace' }
    }
    # Cargo 的 source=null 还包括 path patch；不能把它们冒充 Cortex 自有代码。
    $root = [IO.Path]::GetFullPath($SourceRoot).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    $manifest = [IO.Path]::GetFullPath([string]$Package.manifest_path)
    $properties = @()
    if ($manifest.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
        $properties += @{ name = 'cortex:source'; value = 'local-path' }
        $properties += @{ name = 'cortex:local-source-manifest'; value = $manifest.Substring($root.Length).Replace('\', '/') }
    } else {
        # 外部路径只记录类别，避免把构建用户的目录写进公开 SBOM。
        $properties += @{ name = 'cortex:source'; value = 'external-local-path' }
    }
    $metadata = $Package.PSObject.Properties['metadata']
    if ($null -ne $metadata -and $null -ne $metadata.Value) {
        $vendor = $metadata.Value.PSObject.Properties['cortex_vendor']
        if ($null -ne $vendor -and $null -ne $vendor.Value) {
            $expected = @('upstream_crate_sha256', 'native_backport_revision', 'native_backport_url')
            foreach ($name in $expected) {
                if ($null -eq $vendor.Value.PSObject.Properties[$name] -or
                    $vendor.Value.$name -isnot [string] -or [string]::IsNullOrWhiteSpace($vendor.Value.$name)) {
                    throw "Vendor provenance is incomplete: $($Package.name) / $name"
                }
            }
            $checksum = [string]$vendor.Value.upstream_crate_sha256
            $revision = [string]$vendor.Value.native_backport_revision
            $url = [string]$vendor.Value.native_backport_url
            $uri = $null
            if ($checksum -cnotmatch '^[a-f0-9]{64}$' -or $revision -cnotmatch '^([a-f0-9]{40}|[a-f0-9]{64})$' -or
                -not [Uri]::TryCreate($url, [UriKind]::Absolute, [ref]$uri) -or
                $uri.Scheme -cne 'https' -or $uri.UserInfo -or $uri.Query -or $uri.Fragment) {
                throw "Vendor provenance is invalid: $($Package.name)"
            }
            $properties += @{ name = 'cortex:upstream-crate-sha256'; value = $checksum }
            $properties += @{ name = 'cortex:native-backport-revision'; value = $revision }
            $properties += @{ name = 'cortex:native-backport-url'; value = $url }
        }
    }
    return $properties
}
