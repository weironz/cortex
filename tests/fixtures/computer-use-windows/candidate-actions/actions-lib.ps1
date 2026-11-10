#requires -Version 5.1
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$script:CAHome = $PSScriptRoot
# 只读导入现有隔离/进程/窗口/管道函数，不运行旧 A/B，不改其 AllowedTools。
. (Join-Path $PSScriptRoot '..\readonly-ab\cua-readonly-steady.ps1') -SourceOnly
$script:CATools = @('start_session','get_window_state','set_value','click','end_session')

function Require-CA { param([bool]$Condition, [string]$Code) if (-not $Condition) { throw [InvalidOperationException]::new($Code) } }
function Get-CAFileSha256 {
    param([string]$Path)
    # 与现有 Job 包装相同，避免 Git Bash 的 PSModulePath 让收尾依赖模块加载。
    $stream=[IO.File]::OpenRead($Path); $hash=[Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($hash.ComputeHash($stream)).Replace('-','').ToLowerInvariant() }
    finally { $stream.Dispose(); $hash.Dispose() }
}
function Get-CAError {
    param($Record)
    if ($Record.Exception.Message -cmatch '^(CA|AB)_[A-Z_]+$') { return $Record.Exception.Message }
    return 'CA_RUNTIME_ERROR'
}
function Assert-CAPath {
    param([string]$Path)
    Require-CA ($Path -match '^[A-Za-z]:[\\/]' -and [IO.Path]::IsPathRooted($Path) -and [IO.Path]::GetExtension($Path) -ieq '.exe') 'CA_ABSOLUTE_EXE_REQUIRED'
    Assert-SteadyPathChain ([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($Path)))
}
function Initialize-CANative {
    Initialize-ABNative
    if ('CortexCandidateActions.Native' -as [type]) { return }
    $wpf = Join-Path ([Runtime.InteropServices.RuntimeEnvironment]::GetRuntimeDirectory()) 'WPF'
    Add-Type -Path (Join-Path $script:CAHome 'actions-native.cs') -ReferencedAssemblies @(
        (Join-Path $wpf 'UIAutomationClient.dll'), (Join-Path $wpf 'UIAutomationTypes.dll'), (Join-Path $wpf 'WindowsBase.dll'))
}
function New-CAReport {
    return [ordered]@{
        schema_version=1; scope='unsigned_candidate_fixture_actions_investigation'; status='FAIL'
        investigation_only=$true; product_signature_gate_passed=$false; product_http_fixture_gate_passed=$false
        root_cause='unproven'; failure_code=$null; cleanup_failure_code=$null
        run_name=$null; owner_run_id=$null; harness_pid=$null
        candidate_sha256=$null; fixture_sha256=$null; marker_sha256=$null; fixture_pid=$null; window_id=$null
        version_verified=$false; bounded_status_verified=$false; max_driver_calls=32; daemon_start_attempts=0
        cli_attempts=0; cli_exit_codes=0; cli_zero_exits=0; cli_stdout_chars=0L; cli_stderr_chars=0L
        session_started=$false; end_session_attempted=$false; end_session_verified=$false
        observations=0; set_attempts=0; click_attempts=0; marker_in_input_verified=$false; marker_applied_verified=$false
        native_contract_verified=$false; daemon_cleanup_ok=$false; fixture_cleanup_ok=$false
        fixture_copy_removed=$false; daemon_exit_code=$null; fixture_exit_code=$null
    }
}
function New-CAManifest {
    param([string]$FixtureExecutable)
    return [ordered]@{ version=3; expires_after='3m'; idle_timeout='1m'
        resources=[ordered]@{ apps=@([ordered]@{ executable=$FixtureExecutable; windows='all'; terminate='deny' }) }
        allow=[ordered]@{ tools=@($script:CATools) }
    }
}
function Assert-CAManifest {
    param($Value, [string]$FixtureExecutable)
    $expected = New-CAManifest $FixtureExecutable
    # 本入口只产生这一份固定最小 manifest，拒绝任何增加的资源或工具。
    Require-CA (($Value | ConvertTo-Json -Compress -Depth 8) -ceq ($expected | ConvertTo-Json -Compress -Depth 8)) 'CA_MANIFEST_SCOPE'
    Require-CA ((@($script:AllowedTools) -join ',') -ceq 'start_session,get_window_state,end_session') 'CA_READONLY_TOOLS_CHANGED'
}
function Assert-CAMarker {
    param([string]$Value)
    Require-CA ($Value -cmatch '^CA-ACTIONS-[0-9a-f]{32}$') 'CA_MARKER_FORMAT'
}
function Assert-CAWitness {
    param($Context, $Witness, [string]$InputValue, [string]$StatusValue)
    Require-CA ($Witness.Pid -eq $Context.FixturePid -and $Witness.WindowId -eq $Context.WindowId -and
        $Witness.RootId -ceq 'computer-use-fixture-window' -and $Witness.Framework -ceq 'WPF') 'CA_NATIVE_IDENTITY'
    Require-CA ($Witness.SecureCount -eq 0 -and $Witness.InputPassword -ceq $false -and
        $Witness.InputReadOnly -ceq $false -and $Witness.InputEnabled -ceq $true -and
        $Witness.InputId -ceq '1001' -and $Witness.InputName -ceq 'Message input' -and $Witness.InputMatches -eq 1) 'CA_INPUT_CONTRACT'
    Require-CA ($Witness.ButtonId -ceq '1002' -and $Witness.ButtonName -ceq 'Copy message' -and
        $Witness.ButtonEnabled -ceq $true -and $Witness.ButtonInvoke -ceq $true -and $Witness.ButtonMatches -eq 1) 'CA_APPLY_CONTRACT'
    Require-CA ($Witness.StatusId -ceq '1004' -and $Witness.StatusReadOnly -ceq $true) 'CA_STATUS_CONTRACT'
    Require-CA ($Witness.InputValue -ceq $InputValue -and $Witness.StatusValue -ceq $StatusValue -and
        $Witness.StatusName -ceq $StatusValue) 'CA_NATIVE_READBACK'
}
function Select-CAElement {
    param($State, [string]$Role, [string]$Label, [string]$Action)
    $matches = @($State.elements | Where-Object { (Get-ABField $_ 'role') -ceq $Role -and (Get-ABField $_ 'label') -ceq $Label })
    Require-CA ($matches.Count -eq 1) 'CA_ELEMENT_AMBIGUOUS'
    $element = $matches[0]
    Require-CA ((Get-ABField $element 'enabled') -ceq $true -and
        (Get-ABField $element 'element_token') -cmatch '^s[0-9a-f]{8}:[0-9]+$') 'CA_ELEMENT_TARGET'
    if ($Action) {
        $actions=Get-ABField $element 'actions'
        Require-CA ($actions -is [Array] -and $Action -cin $actions) 'CA_ELEMENT_ACTION'
    }
    return $element
}
function Get-CAObservation {
    param($Context, [string]$InputValue, [string]$StatusValue)
    $arguments = New-ABStateArguments $Context.FixturePid $Context.WindowId $Context.Session
    $reply = Invoke-CATool $Context 'get_window_state' $arguments
    $null = Assert-ABState $reply.Payload $Context.FixturePid $Context.WindowId $Context.Seen
    Require-CA ($reply.Payload.snapshot_id -cmatch '^s[0-9a-f]{8}$') 'CA_SNAPSHOT_FORMAT'
    $Context.Result.observations++
    $state = $reply.Payload
    $input = Select-CAElement $state 'Edit' 'Message input' 'set_value'
    $null = Select-CAElement $state 'Button' 'Copy message' 'invoke'
    $status = Select-CAElement $state 'Edit' $StatusValue ''
    Require-CA ((Get-ABField $input 'value') -ceq $InputValue -and (Get-ABField $status 'value') -ceq $StatusValue) 'CA_STATE_READBACK'
    $witness = & $Context.IO.Read $Context
    Assert-CAWitness $Context $witness $InputValue $StatusValue
    return $state
}
function Assert-CAArguments {
    param($Context, [string]$Tool, $Arguments)
    Require-CA ($Tool -cin $script:CATools -and $Arguments.session -ceq $Context.Session) 'CA_TOOL_SCOPE'
    if ($Tool -in @('start_session','end_session')) {
        Require-CA ($Arguments.Count -eq 1) 'CA_SESSION_ARGUMENTS'
        return
    }
    Require-CA ($Arguments.pid -eq $Context.FixturePid -and $Arguments.window_id -eq $Context.WindowId) 'CA_TARGET_IDENTITY'
    if ($Tool -ceq 'get_window_state') {
        $expected = New-ABStateArguments $Context.FixturePid $Context.WindowId $Context.Session
    }
    else {
        $kind = 'InputToken'
        if ($Tool -ceq 'click') { $kind = 'ButtonToken' }
        Require-CA ($null -ne $Context.$kind -and $Arguments.element_token -ceq $Context.$kind) 'CA_STALE_OR_WRONG_TOKEN'
        $expected = [ordered]@{ session=$Context.Session; pid=$Context.FixturePid; window_id=$Context.WindowId; element_token=$Context.$kind }
        if ($Tool -ceq 'set_value') { Assert-CAMarker $Context.Marker; $expected.value=$Context.Marker; $expected.delivery_mode='background' }
        else { $expected.delivery_mode='background'; $expected.scope='window'; $expected.button='left'; $expected.count=1 }
    }
    Require-CA (($Arguments | ConvertTo-Json -Depth 8 -Compress) -ceq ($expected | ConvertTo-Json -Depth 8 -Compress)) 'CA_ARGUMENT_SHAPE'
}
function Get-CACallBudget {
    param($Context, [bool]$Cleanup)
    # 31 个 CLI 槽加 1 次 serve；普通调用再保留 1 个 end_session 槽。
    $limit = 30
    if ($Cleanup) { $limit = 31 }
    Require-CA ($Context.Result.cli_attempts -lt $limit) 'CA_CALL_BUDGET'
    $left = 90000 - (& $Context.IO.Now $Context)
    if ($Cleanup) { $left = $Context.CallMs }
    Require-CA ($left -gt 0) 'CA_DEADLINE'
    return [int][Math]::Min($Context.CallMs, $left)
}
function Invoke-CATool {
    param($Context, [string]$Tool, $Arguments, [bool]$Cleanup=$false)
    Assert-CAArguments $Context $Tool $Arguments
    & $Context.IO.Check $Context $Cleanup
    $deadline = Get-CACallBudget $Context $Cleanup
    if ($Tool -ceq 'set_value') { Require-CA ($Context.Result.set_attempts -eq 0) 'CA_ACTION_REPLAY'; $Context.Result.set_attempts++ }
    if ($Tool -ceq 'click') { Require-CA ($Context.Result.click_attempts -eq 0) 'CA_ACTION_REPLAY'; $Context.Result.click_attempts++ }
    $reply = & $Context.IO.Call $Context $Tool $Arguments $deadline
    Assert-ABNoImages $reply.Payload
    foreach ($flag in @('isError','is_error','refusal','error','escalation','code')) {
        $v = Get-ABField $reply.Payload $flag
        Require-CA ($null -eq $v -or ($v -is [bool] -and -not $v)) 'CA_TOOL_REFUSED'
    }
    if ($Tool -ceq 'set_value') {
        # 平台工具的文本 ACK 被 tool.rs publish_action_result 装入公共 summary。
        # 该 legacy 路径的实际 delivery 可以诚实地为 unknown，不能伪称已验证。
        $index = ($Arguments.element_token -split ':')[1]
        $expected = [string][char]0x2705 + ' Set AXValue on [' + $index + '] (UIA ValuePattern).'
        Require-CA ((Get-ABField $reply.Payload 'summary') -ceq $expected -and
            (Get-ABField $reply.Payload 'effect') -cin @('unverifiable','confirmed') -and
            (Get-ABField $reply.Payload 'route') -ceq 'accessibility' -and
            (Get-ABField (Get-ABField $reply.Payload 'delivery') 'mode') -cin @('unknown','background')) 'CA_SET_ACK'
    }
    else {
        if ($Tool -ceq 'click') {
            Require-CA ($reply.Payload.effect -cin @('unverifiable','confirmed') -and
                $reply.Payload.route -ceq 'accessibility' -and $reply.Payload.delivery.mode -ceq 'background') 'CA_CLICK_ROUTE'
        }
    }
    return $reply
}
function Invoke-CASequence {
    param($Context)
    $attempted = $false
    try {
        Assert-CAMarker $Context.Marker
        $native = & $Context.IO.Read $Context
        Assert-CAWitness $Context $native $Context.Initial $Context.Baseline
        $Context.Result.native_contract_verified=$true
        $attempted=$true
        $reply = Invoke-CATool $Context 'start_session' ([ordered]@{session=$Context.Session})
        Assert-ABSession $reply.Payload $Context.Session $true
        $Context.Result.session_started=$true
        $state = Get-CAObservation $Context $Context.Initial $Context.Baseline
        $Context.InputToken = (Select-CAElement $state 'Edit' 'Message input' 'set_value').element_token
        $set = [ordered]@{session=$Context.Session;pid=$Context.FixturePid;window_id=$Context.WindowId;element_token=$Context.InputToken;value=$Context.Marker;delivery_mode='background'}
        $null = Invoke-CATool $Context 'set_value' $set
        $state = Get-CAObservation $Context $Context.Marker $Context.Baseline
        $Context.InputToken=$null
        $Context.Result.marker_in_input_verified=$true
        $Context.ButtonToken = (Select-CAElement $state 'Button' 'Copy message' 'invoke').element_token
        $click = [ordered]@{session=$Context.Session;pid=$Context.FixturePid;window_id=$Context.WindowId;element_token=$Context.ButtonToken;delivery_mode='background';scope='window';button='left';count=1}
        $null = Invoke-CATool $Context 'click' $click
        $null = Get-CAObservation $Context $Context.Marker $Context.Marker
        $Context.ButtonToken=$null
        $Context.Result.marker_applied_verified=$true
    }
    finally {
        if ($attempted) {
            $Context.Result.end_session_attempted=$true
            try {
                $reply = Invoke-CATool $Context 'end_session' ([ordered]@{session=$Context.Session}) $true
                Assert-ABSession $reply.Payload $Context.Session $false
                $Context.Result.end_session_verified=$true
            }
            catch { $Context.Result.cleanup_failure_code=Get-CAError $_ }
        }
    }
}
function Assert-CACompleted {
    param($Value)
    Require-CA ($null -eq $Value.failure_code -and $null -eq $Value.cleanup_failure_code -and
        $Value.investigation_only -ceq $true -and $Value.product_signature_gate_passed -ceq $false -and
        $Value.product_http_fixture_gate_passed -ceq $false -and
        $Value.version_verified -ceq $true -and $Value.bounded_status_verified -ceq $true -and
        $Value.session_started -ceq $true -and $Value.end_session_verified -ceq $true -and
        $Value.observations -eq 3 -and $Value.set_attempts -eq 1 -and $Value.click_attempts -eq 1 -and
        $Value.marker_in_input_verified -ceq $true -and $Value.marker_applied_verified -ceq $true -and
        $Value.native_contract_verified -ceq $true -and $Value.cli_attempts -eq 9 -and
        $Value.cli_zero_exits -eq $Value.cli_attempts -and $Value.cli_exit_codes -eq $Value.cli_attempts -and
        $Value.daemon_start_attempts -eq 1 -and $Value.cli_attempts + $Value.daemon_start_attempts -le 32 -and
        $Value.daemon_cleanup_ok -ceq $true -and (Test-ABInteger $Value.daemon_exit_code) -and
        $Value.fixture_cleanup_ok -ceq $true -and (Test-ABInteger $Value.fixture_exit_code) -and $Value.fixture_exit_code -eq 0 -and
        $Value.fixture_copy_removed -ceq $true) 'CA_PROBE_INCOMPLETE'
}
