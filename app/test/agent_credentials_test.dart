import 'package:flutter_test/flutter_test.dart';
import 'package:cortex_app/core/agent_credentials.dart';

void main() {
  test('设备控制凭据不会在未登录时冒充远端token', () {
    final env = AgentLaunchCredentials(
      inboundToken: 'local-test-control',
    ).environment(null);
    expect(env['CORTEX_INBOUND_TOKEN'], 'local-test-control');
    expect(env['CORTEX_TOKEN'], '', reason: '显式清空父进程可能遗留的账号凭据');
  });

  test('有账号时仍使用两条独立认证路径', () {
    final env = AgentLaunchCredentials(
      inboundToken: 'local-test-control',
      userToken: 'remote-test-access',
    ).environment(null);
    expect(env['CORTEX_INBOUND_TOKEN'], 'local-test-control');
    expect(env['CORTEX_TOKEN'], 'remote-test-access');
  });

  test('模型环境大小写变体不能覆盖凭据且不修改调用者map', () {
    final extra = Map<String, String>.unmodifiable({
      'cortex_token': 'untrusted-test',
      'CoRtEx_InBoUnD_ToKeN': 'untrusted-test',
      'CORTEX_LLM_MODEL': 'test-model',
    });
    final env = AgentLaunchCredentials(
      inboundToken: 'local-test-control',
    ).environment(extra);
    expect(env.keys.where((key) => key.toUpperCase() == 'CORTEX_TOKEN'), [
      'CORTEX_TOKEN',
    ]);
    expect(
      env.keys.where((key) => key.toUpperCase() == 'CORTEX_INBOUND_TOKEN'),
      ['CORTEX_INBOUND_TOKEN'],
    );
    expect(env['CORTEX_LLM_MODEL'], 'test-model');
    expect(extra.length, 3, reason: '不能修改调用者持有的不可变配置');
  });

  test('拒绝空控制凭据及意外复用账号凭据', () {
    expect(
      () => AgentLaunchCredentials(inboundToken: ' '),
      throwsArgumentError,
    );
    expect(
      () => AgentLaunchCredentials(
        inboundToken: 'same-test',
        userToken: 'same-test',
      ),
      throwsArgumentError,
    );
  });
}
