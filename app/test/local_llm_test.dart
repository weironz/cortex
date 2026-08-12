/// 本机 LLM 配置：离线模式下，桌面端怎么告诉本地 agent「打给谁」。
///
/// # 在此之前这件事只能靠环境变量
///
/// 离线模式里没有 cortexd，代理无处可代，本地 agent 必须自己直连。
/// 而它此前只能从**启动桌面端之前**设好的环境变量里拿这些值 ——
/// 对一个双击图标的人来说等于没有。
///
/// 这几条盯的都是「改了不会报错、只会静默变糟」的地方：环境变量名拼错、
/// 空串顶掉默认值、以及把 token 覆盖掉。
library;

import 'package:cortex_app/core/local_llm.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('环境变量名与 cortexd / cortex-local 读的完全一致', () {
    const cfg = LocalLlmConfig(
      provider: 'deepseek',
      apiKey: 'sk-abc',
      baseUrl: 'http://gw.local/v1',
      model: 'deepseek-chat',
    );
    final env = cfg.toEnvironment();

    expect(env['CORTEX_LLM_PROVIDER'], 'deepseek');
    expect(env['CORTEX_LLM_BASE_URL'], 'http://gw.local/v1');
    expect(env['CORTEX_LLM_MODEL'], 'deepseek-chat');
    expect(
      env['DEEPSEEK_API_KEY'],
      'sk-abc',
      reason:
          'key 的变量名由供应商决定。拼错的话 agent 找不到 key，'
          '报的却是「缺少环境变量」—— 而用户明明在界面上填过了',
    );
  });

  test('空值一律不塞 —— 空串会顶掉下游的默认值', () {
    const cfg = LocalLlmConfig(provider: 'ollama', model: '   ');
    final env = cfg.toEnvironment();

    expect(env.containsKey('CORTEX_LLM_PROVIDER'), isTrue);
    expect(
      env.containsKey('CORTEX_LLM_MODEL'),
      isFalse,
      reason:
          '`env::var` 对空串返回 Ok("")，会把供应商定义里的默认模型顶掉 —— '
          '然后请求带着一个空模型名发出去。这个坑在这个仓库出现过四次',
    );
    expect(
      env.containsKey('CORTEX_LLM_BASE_URL'),
      isFalse,
      reason: '空的 base_url 会让请求打到一个拼不出来的地址上',
    );
    expect(
      env.keys.any((k) => k.endsWith('_API_KEY')),
      isFalse,
      reason:
          '免鉴权的端点（本机 ollama）不该被塞一个空 key —— '
          'cortex-llm 用 api_key_env 是否为空来判断免不免鉴权',
    );
  });

  test('只要 provider 就算可用 —— key 是否必需由服务端说了算', () {
    expect(
      const LocalLlmConfig(provider: 'ollama').isUsable,
      isTrue,
      reason:
          '本机 ollama 免 key。在客户端抄一份「哪些供应商要 key」的判断，'
          '迟早与 cortex-llm 的 api_key_env 漂开',
    );
    expect(const LocalLlmConfig(apiKey: 'sk-only').isUsable, isFalse);
    expect(LocalLlmConfig.empty.isUsable, isFalse);
  });

  test('供应商名里的横杠转成下划线（环境变量不允许横杠）', () {
    final env = const LocalLlmConfig(
      provider: 'my-gateway',
      apiKey: 'k',
    ).toEnvironment();
    expect(
      env['MY_GATEWAY_API_KEY'],
      'k',
      reason:
          '`MY-GATEWAY_API_KEY` 在多数 shell 与 Windows 上根本设不了，'
          '而失败方式是「变量不存在」—— 与没填过一模一样',
    );
  });

  test('JSON 往返不丢字段', () {
    const cfg = LocalLlmConfig(
      provider: 'openai',
      apiKey: 'sk-1',
      baseUrl: 'http://x',
      model: 'm',
    );
    expect(LocalLlmConfig.fromJson(cfg.toJson()), cfg);
  });
}
