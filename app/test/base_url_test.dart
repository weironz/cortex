/// 空的 cortexd 地址意味着什么。
///
/// # 为什么值得单独一个文件
///
/// 这一条把 `just dev` 那套拓扑整个卡死过一次，而症状指向了完全错误的方向：
/// 界面报「连不上 cortexd（http://127.0.0.1:8080）。确认 daemon 已启动」，
/// 而 daemon 好好地跑着 —— 真相是**浏览器压根不该去 8080**。
///
/// dev 的拓扑是「浏览器 → nginx:5173 → cortexd」，同源反代，构建时用
/// `--dart-define=CORTEX_BASE_URL=`（空串）表达「用当前这个源」。而
/// `_normalise` 把空串换成了 `http://127.0.0.1:8080` —— 另一个源，于是
/// 浏览器按 CORS 把响应丢掉，抛的是 `TypeError: Failed to fetch`。
///
/// 本仓库第 7 次「空串顶掉默认值」，方向反过来：前六次是空串被当成
/// 「配过了」，这次是一个**刻意的空串**被当成「没配过」。
library;

import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('空地址', () {
    test('浏览器里是「同源」，不是 127.0.0.1:8080', () {
      expect(
        HttpCortexApi.resolveBase('', Uri.parse('http://127.0.0.1:5173/')),
        Uri.parse('http://127.0.0.1:5173'),
        reason:
            'dev 与生产都是同源反代（nginx / traefik），空串是**有意义的配置**。'
            '换成另一个源之后浏览器按 CORS 丢掉响应，报的是 Failed to fetch —— '
            '一句看起来像 daemon 没起来的错，而 daemon 好好跑着',
      );
    });

    test('页面路径与查询串不进 API 根', () {
      expect(
        HttpCortexApi.resolveBase(
          '',
          Uri.parse('https://cortex.example.com/app/index.html?x=1#frag'),
        ),
        Uri.parse('https://cortex.example.com'),
        reason:
            'API 根只有 scheme+host+port。带上页面路径的话，'
            '/health 会被拼成 /app/index.html/health',
      );
    });

    test('桌面端（没有页面源）才回落到本机 daemon', () {
      expect(
        HttpCortexApi.resolveBase('', Uri.parse('file:///C:/codes/cortex/')),
        Uri.parse('http://127.0.0.1:8080'),
        reason:
            '桌面端的 Uri.base 是进程工作目录，没有同源可言 —— '
            '那儿的空串确实是「没配」，回落到本机默认端口是对的。'
            '判据是有没有页面源，不是 kIsWeb',
      );
    });
  });

  group('给了地址就照用', () {
    test('补 http:// 前缀、削掉结尾斜杠', () {
      final page = Uri.parse('http://127.0.0.1:5173/');
      expect(
        HttpCortexApi.resolveBase('cortex.example.com:8080/', page),
        Uri.parse('http://cortex.example.com:8080'),
      );
      expect(
        HttpCortexApi.resolveBase('  https://a.example.com//  ', page),
        Uri.parse('https://a.example.com'),
      );
    });

    test('显式地址压过同源 —— 浏览器里也一样', () {
      expect(
        HttpCortexApi.resolveBase(
          'http://127.0.0.1:8080',
          Uri.parse('http://127.0.0.1:5173/'),
        ),
        Uri.parse('http://127.0.0.1:8080'),
        reason:
            'just run 那条拓扑（cortexd 跑在宿主、浏览器直连、开 CORS）'
            '仍然要能用 —— 用户在登录框里填了什么就打什么',
      );
    });
  });
}
