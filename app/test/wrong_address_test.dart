@TestOn('vm')
library;

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

/// 填错地址是**第一次使用最可能的失败**，而它原来的报错帮不上任何忙。
///
/// 自托管部署里 `https://host` 是 Web 界面、`https://host/api` 才是 cortexd。
/// 用户理所当然会填前者（那是他在浏览器里访问的地址），于是 `jsonDecode`
/// 拿到一整页 HTML 并抛出：
///
/// ```
/// FormatException: Unexpected character (at character 1)
/// <!DOCTYPE html>
/// ^
/// ```
///
/// 这句话技术上完全正确，而且**一个字都没告诉用户该怎么办**。
/// 它原样出现在了登录屏上（真实截图）。
void main() {
  const html = '<!DOCTYPE html>\n<html><head><title>Cortex</title></head></html>';

  MockClient serving(String body, {String contentType = 'text/html'}) =>
      MockClient(
        (_) async => http.Response(
          body,
          200,
          headers: {'content-type': contentType},
        ),
      );

  group('填错地址', () {
    test('拿到网页时说清是网页，并给出带 /api 的那个地址', () async {
      final api = HttpCortexApi(
        baseUrl: 'https://cortex.example.com',
        token: 't',
        client: serving(html),
      );
      addTearDown(api.dispose);

      final e = await api.issueTicket().then<Object?>(
        (_) => null,
        onError: (Object e) => e,
      );

      expect(e, isA<CortexApiException>());
      final msg = e.toString();
      expect(
        msg,
        contains('网页'),
        reason: '必须点明拿到的是网页 —— 「Unexpected character」说的是同一件事，'
            '但用户读不出来',
      );
      expect(
        msg,
        contains('https://cortex.example.com/api'),
        reason: '必须给出**具体**的地址，而不是描述问题的形状。'
            '这是用户下一步唯一要做的事',
      );
      expect(
        msg,
        isNot(contains('Unexpected character')),
        reason: '原始 FormatException 不该再冒到界面上',
      );
    });

    test('地址里已经有路径时不乱猜 /api', () async {
      final api = HttpCortexApi(
        baseUrl: 'https://cortex.example.com/api',
        token: 't',
        client: serving(html),
      );
      addTearDown(api.dispose);

      final e = await api.issueTicket().then<Object?>(
        (_) => null,
        onError: (Object e) => e,
      );
      final msg = e.toString();

      expect(msg, contains('网页'));
      expect(
        msg,
        isNot(contains('/api/api')),
        reason: '已经有路径还建议再加一层 /api，比不给建议更糟 —— '
            '用户会照着改，然后错得更远',
      );
    });

    test('不是 HTML 的垃圾响应，把原文带出来', () async {
      final api = HttpCortexApi(
        baseUrl: 'https://cortex.example.com/api',
        token: 't',
        client: serving('not json at all', contentType: 'text/plain'),
      );
      addTearDown(api.dispose);

      final e = await api.issueTicket().then<Object?>(
        (_) => null,
        onError: (Object e) => e,
      );
      expect(
        e.toString(),
        contains('not json at all'),
        reason: '认不出的响应必须带上原文 —— 那是排查时唯一的线索',
      );
    });
  });
}
