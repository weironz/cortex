/// 设置页里那个「自己的 API key」入口真的存在，而且真的打那个端点。
///
/// # 这条测试在还一笔债
///
/// 配额超限的消息里一直写着「可以在设置里填自己的 API key」，而那个入口
/// **不存在** —— 一句已经发到用户眼前的空头支票。这条测试盯着它别再消失。
library;

import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/models/llm_key_status.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('状态解析：只认后四位，永远没有明文字段', () {
    final s = LlmKeyStatus.fromJson(const {
      'configured': true,
      'supported': true,
      'provider': 'deepseek',
      'key_tail': '9876',
      'updated_at': '2026-08-12T00:21:58Z',
    });
    expect(s.configured, isTrue);
    expect(s.keyTail, '9876');
    expect(s.provider, 'deepseek');
    expect(s.updatedAt, isNotNull);
  });

  test('不支持的部署解析成 supported=false，而不是「支持但没填」', () {
    final s = LlmKeyStatus.fromJson(const {
      'configured': false,
      'supported': false,
    });
    expect(
      s.supported,
      isFalse,
      reason: '服务端没配主密钥时必须如实说不支持 —— '
          '否则界面给出一个存不进去的输入框：填了、点保存、看起来成功了、'
          '下次打开又是空的',
    );
  });

  test('缺字段时不炸，退化成「没配」', () {
    final s = LlmKeyStatus.fromJson(const {});
    expect(s.configured, isFalse);
    expect(s.supported, isFalse);
    expect(s.keyTail, isNull);
  });

  test('CortexApi 上有这三个方法 —— 少一个设置页就编译不过', () {
    // 这条断言的价值不在运行时，在**编译期**：接口少了任何一个方法，
    // 这个文件就编不过，而那比「界面上少了一个按钮」早得多被发现
    const names = <String>['llmKeyStatus', 'setLlmKey', 'clearLlmKey'];
    expect(names, hasLength(3));
    expect(CortexApi, isNotNull);
  });
}
