/// 错误信封要在**进界面之前**剥掉。
///
/// # 这条测试在还一笔债
///
/// 服务端一律把错误包成 `{"error": "…"}`。非流式那条路一直会剥，流式那条
/// （`/chat`、重挂、导入）**不剥** —— 于是 2026-08-15 桌面端的聊天气泡里
/// 画出来的是
///
/// ```
/// {"error":"这一轮要在云端跑，但数据源 … 请把数据源改成部署入口。"}
/// ```
///
/// 那句提示本身是特地为这个场景写的，结果因为多了一层信封，用户看到的
/// 结论是「这个报错很不友好」。**一条写得再好的错误信息，只要露出实现
/// 细节就等于没写。**
library;

import 'dart:async';
import 'dart:convert';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

/// 一个只会拒绝的后端：任何请求都回给定的状态码与 body。
http.Client _rejecting(int status, String body) =>
    MockClient.streaming((request, bodyStream) async {
      return http.StreamedResponse(
        Stream.value(utf8.encode(body)),
        status,
        request: request,
        headers: const {'content-type': 'application/json'},
      );
    });

void main() {
  const wrapped =
      '这一轮要在云端跑，但数据源 http://127.0.0.1:8080 上没有 /chat。'
      '请把数据源改成部署入口。';

  test('/chat 的错误剥掉 {"error": …} 信封', () async {
    final api = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:1',
      client: _rejecting(502, jsonEncode({'error': wrapped})),
    );
    addTearDown(api.dispose);

    await expectLater(
      api.chat(sessionId: 'S1', message: 'hi').toList(),
      throwsA(
        isA<CortexApiException>()
            .having((e) => e.message, '消息', wrapped)
            .having((e) => e.message, '不含信封', isNot(contains('{"error"'))),
      ),
    );
  });

  test('重挂那条也剥 —— 两条路共用同一个解析', () async {
    final api = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:1',
      client: _rejecting(502, jsonEncode({'error': wrapped})),
    );
    addTearDown(api.dispose);

    await expectLater(
      api.attachChat('S1').toList(),
      throwsA(
        isA<CortexApiException>().having((e) => e.message, '消息', wrapped),
      ),
    );
  });

  /// 不是 JSON 的 body **照原样给**：反代与网关回的常是纯文本
  /// （nginx 的 `502 Bad Gateway` 页面之类），那些原文同样是线索。
  test('非 JSON 的 body 原样保留', () async {
    final api = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:1',
      client: _rejecting(502, 'upstream connect error'),
    );
    addTearDown(api.dispose);

    await expectLater(
      api.chat(sessionId: 'S1', message: 'hi').toList(),
      throwsA(
        isA<CortexApiException>().having(
          (e) => e.message,
          '消息',
          contains('upstream connect error'),
        ),
      ),
    );
  });

  /// body 为空时**必须自己编一句**：空串会让界面画出一个只有红框没有字的
  /// 提示，用户知道出错了却拿不到任何线索。状态码是那时唯一剩下的信息。
  test('空 body 时给出带状态码的兜底', () async {
    final api = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:1',
      client: _rejecting(502, ''),
    );
    addTearDown(api.dispose);

    await expectLater(
      api.chat(sessionId: 'S1', message: 'hi').toList(),
      throwsA(
        isA<CortexApiException>().having(
          (e) => e.message,
          '消息',
          allOf(isNotEmpty, contains('502')),
        ),
      ),
    );
  });
}
