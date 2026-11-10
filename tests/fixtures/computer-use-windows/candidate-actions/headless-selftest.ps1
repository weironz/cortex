#requires -Version 5.1
function New-CAMemoryContext {
    param([string]$Failure='')
    $report=New-CAReport
    $report.version_verified=$true; $report.bounded_status_verified=$true; $report.daemon_start_attempts=1
    $report.cli_attempts=2; $report.cli_exit_codes=2; $report.cli_zero_exits=2
    return [pscustomobject]@{
        Result=$report; FixturePid=123; WindowId=456L; CallMs=10000; Session='headless-only-session'
        Marker=('CA-ACTIONS-'+('a'*32)); Initial='CA-INITIAL-synthetic'; Baseline='CA-READY-synthetic'
        InputToken=$null; ButtonToken=$null; Seen=[Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        Memory=@{Failure=$Failure;Input='CA-INITIAL-synthetic';Status='CA-READY-synthetic';Snapshot=0;Now=0L;Calls=[Collections.Generic.List[string]]::new()}
        IO=@{
            Now={param($c) return $c.Memory.Now}
            Check={param($c,$cleanup) if ($c.Memory.Failure -ceq 'identity') { throw 'AB_PROCESS_IDENTITY_PID' }}
            Read={
                param($c)
                $v=[pscustomobject]@{Pid=123;WindowId=456L;RootId='computer-use-fixture-window';Framework='WPF'
                    InputId='1001';InputName='Message input';InputValue=$c.Memory.Input;InputReadOnly=$false;InputPassword=$false;InputEnabled=$true;InputMatches=1
                    ButtonId='1002';ButtonName='Copy message';ButtonEnabled=$true;ButtonInvoke=$true;ButtonMatches=1
                    StatusId='1004';StatusName=$c.Memory.Status;StatusValue=$c.Memory.Status;StatusReadOnly=$true;SecureCount=0}
                switch ($c.Memory.Failure) {
                    'secure' {$v.SecureCount=1;$v.InputPassword=$true}
                    'readonly' {$v.InputReadOnly=$true}
                    'wrong_id' {$v.InputId='1003'}
                    'wrong_button' {$v.ButtonName='Apply elsewhere'}
                    'not_readonly_status' {$v.StatusReadOnly=$false}
                    'native_pid' {$v.Pid=124}
                }
                return $v
            }
            Call={
                param($c,$tool,$arguments,$deadline)
                Require-CA ($deadline -gt 0 -and $deadline -le 10000) 'CA_SELFTEST_UNBOUNDED_CALL'
                $c.Result.cli_attempts++; $c.Memory.Calls.Add($tool); $c.Memory.Now+=10
                if ($c.Memory.Failure -ceq $tool) { throw 'CA_SYNTHETIC_CALL_FAILED' }
                $c.Result.cli_exit_codes++; $c.Result.cli_zero_exits++
                $payload=$null
                switch ($tool) {
                    'start_session' {$payload=@{session=$c.Session;active=$true}}
                    'end_session' {$payload=@{session=$c.Session;active=$false}}
                    'set_value' {
                        if ($c.Memory.Failure -cne 'set_noop') {$c.Memory.Input=$arguments.value}
                        $index=($arguments.element_token -split ':')[1]
                        $payload=@{effect='unverifiable';route='accessibility';delivery=@{mode='unknown'}
                            summary=([string][char]0x2705+' Set AXValue on ['+$index+'] (UIA ValuePattern).')}
                        if ($c.Memory.Failure -ceq 'bad_ack') {$payload.summary='unknown'}
                    }
                    'click' {
                        if ($c.Memory.Failure -cne 'click_noop') {$c.Memory.Status=$c.Memory.Input}
                        $payload=@{effect='unverifiable';route='accessibility';delivery=@{mode='background'}}
                        if ($c.Memory.Failure -ceq 'foreground') {$payload.delivery.mode='foreground'}
                    }
                    'get_window_state' {
                        $c.Memory.Snapshot++
                        $number=$c.Memory.Snapshot
                        if ($c.Memory.Failure -ceq 'replay') {$number=1}
                        $snapshot='s'+$number.ToString('x8')
                        $elements=@(
                            @{element_index=0;element_token=($snapshot+':0');role='Edit';label='Message input';value=$c.Memory.Input;enabled=$true;actions=@('set_value')},
                            @{element_index=1;element_token=($snapshot+':1');role='Button';label='Copy message';enabled=$true;actions=@('invoke')},
                            @{element_index=2;element_token=($snapshot+':2');role='Edit';label=$c.Memory.Status;value=$c.Memory.Status;enabled=$true;actions=@('set_value')})
                        if ($c.Memory.Failure -ceq 'duplicate') {$elements+=@{element_index=3;element_token=($snapshot+':3');role='Edit';label='Message input';enabled=$true;actions=@('set_value')}}
                        if ($c.Memory.Failure -ceq 'disabled') {$elements[1].enabled=$false}
                        if ($c.Memory.Failure -ceq 'no_action') {$elements[0].actions=@('text')}
                        if ($c.Memory.Failure -ceq 'token') {$elements[0].element_token='other:0'}
                        $payload=@{pid=123;window_id=456L;window_title='Cortex Computer Use Fixture';snapshot_id=$snapshot
                            tree_format='both';tree_markdown='synthetic-memory-only';elements_complete=$true;truncated=$false
                            total_element_count=$elements.Count;returned_element_count=$elements.Count;elements=$elements}
                        if ($c.Memory.Failure -ceq 'pid') {$payload.pid=124}
                        if ($c.Memory.Failure -ceq 'hwnd') {$payload.window_id=457L}
                        if ($c.Memory.Failure -ceq 'image') {$payload.content=@(@{type='image';data='synthetic'})}
                        if ($c.Memory.Failure -ceq 'error_string') {$payload.error='False'}
                    }
                    default {throw 'CA_SELFTEST_UNEXPECTED_TOOL'}
                }
                return [pscustomobject]@{Payload=$payload;Text=''}
            }
        }
    }
}
function Invoke-CAHeadlessSelfTests {
    $checks=0
    function Expect-CAReject {
        param([scriptblock]$Action,[string]$Code)
        $rejected=$false
        try {$null=&$Action}
        catch {Require-CA ($_.Exception.Message -ceq $Code) 'CA_SELFTEST_WRONG_REJECTION';$rejected=$true}
        Require-CA $rejected 'CA_SELFTEST_EXPECTED_REJECTION'
    }
    $c=New-CAMemoryContext
    Invoke-CASequence $c
    Require-CA (($c.Memory.Calls -join ',') -ceq 'start_session,get_window_state,set_value,get_window_state,click,get_window_state,end_session' -and
        $c.Result.cli_attempts -eq 9 -and $c.Result.marker_in_input_verified -and $c.Result.marker_applied_verified) 'CA_SELFTEST_FIXED_FLOW'
    $checks++
    Expect-CAReject {Assert-CACompleted $c.Result} 'CA_PROBE_INCOMPLETE';$checks++
    $c.Result.daemon_cleanup_ok=$true;$c.Result.daemon_exit_code=-1
    $c.Result.fixture_cleanup_ok=$true;$c.Result.fixture_exit_code=0;$c.Result.fixture_copy_removed=$true
    Assert-CACompleted $c.Result;$checks++
    $failures=@{secure='CA_INPUT_CONTRACT';readonly='CA_INPUT_CONTRACT';wrong_id='CA_INPUT_CONTRACT';wrong_button='CA_APPLY_CONTRACT'
        not_readonly_status='CA_STATUS_CONTRACT';native_pid='CA_NATIVE_IDENTITY';pid='AB_STATE_PID';hwnd='AB_STATE_HWND'
        image='AB_IMAGE_CONTENT';error_string='CA_TOOL_REFUSED';token='AB_ELEMENT_TOKEN';duplicate='CA_ELEMENT_AMBIGUOUS';disabled='CA_ELEMENT_TARGET'
        no_action='CA_ELEMENT_ACTION';replay='AB_SNAPSHOT_REUSED';set_noop='CA_STATE_READBACK';click_noop='CA_ELEMENT_AMBIGUOUS'
        bad_ack='CA_SET_ACK';foreground='CA_CLICK_ROUTE';identity='AB_PROCESS_IDENTITY_PID'
        start_session='CA_SYNTHETIC_CALL_FAILED';set_value='CA_SYNTHETIC_CALL_FAILED';click='CA_SYNTHETIC_CALL_FAILED'}
    foreach ($name in $failures.Keys) {
        $bad=New-CAMemoryContext $name
        Expect-CAReject {Invoke-CASequence $bad} $failures[$name]
        Require-CA ($bad.Result.set_attempts -le 1 -and $bad.Result.click_attempts -le 1 -and
            @($bad.Memory.Calls|Where-Object {$_ -ceq 'end_session'}).Count -le 1) 'CA_SELFTEST_NO_REPLAY'
        $checks+=2
    }
    $bad=New-CAMemoryContext 'end_session';Invoke-CASequence $bad
    Require-CA (-not $bad.Result.end_session_verified -and $bad.Result.cleanup_failure_code -ceq 'CA_SYNTHETIC_CALL_FAILED') 'CA_SELFTEST_END_FAILURE'
    $checks++
    $bad=New-CAMemoryContext;$bad.Result.cli_attempts=30
    Expect-CAReject {Get-CACallBudget $bad $false} 'CA_CALL_BUDGET';$checks++
    $null=Get-CACallBudget $bad $true;$checks++
    $bad.Result.cli_attempts=31
    Expect-CAReject {Get-CACallBudget $bad $true} 'CA_CALL_BUDGET';$checks++
    $bad=New-CAMemoryContext;$bad.Memory.Now=90000
    Expect-CAReject {Get-CACallBudget $bad $false} 'CA_DEADLINE';$checks++
    foreach ($field in @('cli_zero_exits','observations','set_attempts','click_attempts','end_session_verified','fixture_copy_removed','marker_applied_verified',
            'fixture_exit_code','investigation_only','product_signature_gate_passed')) {
        $record=[ordered]@{};foreach($entry in $c.Result.GetEnumerator()){$record[$entry.Key]=$entry.Value};$record[$field]=$null
        Expect-CAReject {Assert-CACompleted $record} 'CA_PROBE_INCOMPLETE';$checks++
    }
    $manifest=New-CAManifest 'C:\private\fixture.exe';Assert-CAManifest $manifest 'C:\private\fixture.exe';$checks++
    $manifest.allow.tools+='type_text'
    Expect-CAReject {Assert-CAManifest $manifest 'C:\private\fixture.exe'} 'CA_MANIFEST_SCOPE';$checks++
    $manifest=New-CAManifest 'C:\private\fixture.exe';$manifest.resources.desktop=@{display='all'}
    Expect-CAReject {Assert-CAManifest $manifest 'C:\private\fixture.exe'} 'CA_MANIFEST_SCOPE';$checks++
    $a=New-CAMemoryContext;$a.InputToken='s00000001:0'
    $arguments=[ordered]@{session=$a.Session;pid=123;window_id=456L;element_token=$a.InputToken;value=$a.Marker;delivery_mode='background'}
    Assert-CAArguments $a 'set_value' $arguments;$checks++
    $arguments.scope='window'
    Expect-CAReject {Assert-CAArguments $a 'set_value' $arguments} 'CA_ARGUMENT_SHAPE';$checks++
    $arguments.Remove('scope');$arguments.delivery_mode='foreground'
    Expect-CAReject {Assert-CAArguments $a 'set_value' $arguments} 'CA_ARGUMENT_SHAPE';$checks++
    $arguments.delivery_mode='background';$arguments.element_token='s00000002:0'
    Expect-CAReject {Assert-CAArguments $a 'set_value' $arguments} 'CA_STALE_OR_WRONG_TOKEN';$checks++
    Expect-CAReject {Assert-CAArguments $a 'clipboard_write' @{session=$a.Session}} 'CA_TOOL_SCOPE';$checks++
    Expect-CAReject {Assert-CAMarker 'untrusted text'} 'CA_MARKER_FORMAT';$checks++
    Require-CA (($script:AllowedTools -join ',') -ceq 'start_session,get_window_state,end_session') 'CA_SELFTEST_OLD_CATALOG_CHANGED';$checks++
    Write-Output ('CA_HEADLESS_SELFTEST_PASS checks='+$checks+' flow=fixed set_max=1 click_max=1 calls_max=32 native_calls=0 gui_driver_started=0')
}
