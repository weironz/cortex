/// 云沙箱的文件树 / 上传 / 下载。
///
/// # 这几条各自盯着什么
///
/// 1. **服务端那句话原样透出**。它是唯一说清了「为什么」的东西；界面在前面
///    拼一句自己编的「加载失败」，用户的下一步就从「照那句话做」变成
///    「去查网络和后端」。
///
/// 2. **树是懒加载的**。每展开一层是一次网络往返；急切递归的话，一个装着
///    `node_modules` 的工作区会打出上千个请求。这条断了不会报错，只会
///    「有时候特别慢」，而那种回归没人查得出来。
///
/// 3. **服务端的中文能活着到界面上**。`package:http` 的 `Response.body`
///    在没有 charset 的 `application/json` 上回退到 **latin1** —— 第 1 条
///    那句话会变成乱码，恰好是最需要看清的那一句。
///
/// # 这里曾经有一组「容器不在」
///
/// 它验的是 409 与 501 两条路的措辞不能混（一个发条消息就好、一个发一万条
/// 也没用）。409 那条今天不存在了：列目录自己会把容器拉起来。留下的只有
/// 501 ——「这个部署压根没开云沙箱」，那是真正的永久缺失。
library;

import 'dart:convert';
import 'dart:typed_data';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/local_agent.dart';
import 'package:cortex_app/features/workspace/sandbox_file_tree.dart';
import 'package:cortex_app/models/workspace.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

FileNode _dir(String path) =>
    FileNode(name: basenameOf(path), path: path, isDirectory: true);

FileNode _file(String path, int size) => FileNode(
  name: basenameOf(path),
  path: path,
  isDirectory: false,
  sizeBytes: size,
);

/// 服务端在没开云沙箱时给的那句话。测试里逐字比对，所以它长什么样就写什么样。
const String _kNoSandbox = '这个部署没有开云沙箱（cortexd 连不上 docker）。'
    '要在自己的机器上跑文件与命令，请用桌面端。';

/// 记下每一次列目录的请求 —— 「只要了这一层」这件事只能这样断言。
class _TreeApi extends MockCortexApi {
  _TreeApi(this.tree);

  final Map<String, List<FileNode>> tree;
  final List<String> listed = [];
  final List<String> read = [];

  @override
  Future<List<FileNode>> sandboxListFiles(String path) async {
    listed.add(path);
    final nodes = tree[path];
    if (nodes == null) {
      throw CortexApiException('$path 不存在', statusCode: 404);
    }
    return nodes;
  }

  @override
  Future<Uint8List> sandboxReadFile(String path) async {
    read.add(path);
    // 刻意抛：真去下载会走到 `saveBytesAs`，而它在桌面端**真的会往
    // ~/Downloads 写文件**。这里要验的是「点文件去读的是哪条路径」，
    // 顺带把下载失败时的原样透出也一起验了
    throw const CortexApiException('读不了', statusCode: 500);
  }
}

/// 沙箱**整个没开**的部署 —— 服务端回 501。
///
/// 这是文件树唯一还会失败到「一句话 + 重试」的路：501 是永久缺失，
/// 重试永远不会成功，所以那句话必须自己把原因说清楚。
class _NoSandboxApi extends MockCortexApi {
  @override
  Future<List<FileNode>> sandboxListFiles(String path) async =>
      throw const CortexApiException('这个部署没有开云沙箱', statusCode: 501);
}

Widget _wrap(Widget child, CortexApi api) => ProviderScope(
  overrides: [cortexApiProvider.overrideWithValue(api)],
  child: MaterialApp(home: Scaffold(body: child)),
);

/// 三层，够验「展开一层只要一层」。
Map<String, List<FileNode>> _threeLevels() => {
  kSandboxRoot: [
    _dir('$kSandboxRoot/src'),
    _file('$kSandboxRoot/README.md', 12),
  ],
  '$kSandboxRoot/src': [
    _dir('$kSandboxRoot/src/util'),
    _file('$kSandboxRoot/src/main.py', 30),
  ],
  '$kSandboxRoot/src/util': [_file('$kSandboxRoot/src/util/io.py', 9)],
};

void main() {
  group('打不开的时候', () {
    testWidgets('服务端那句话原样透出，不被改写成「加载失败」', (tester) async {
      await tester.pumpWidget(_wrap(const SandboxBrowser(), _NoSandboxApi()));
      await tester.pumpAndSettle();

      expect(
        find.text('这个部署没有开云沙箱'),
        findsOneWidget,
        reason:
            '这句话是服务端专门写给用户的，它说清了「为什么」。换成任何一句'
            '自己编的话，用户就会去查网络、查后端、以为文件没了',
      );
      expect(
        find.textContaining('加载失败'),
        findsNothing,
        reason: '标题已经说了打不开，正文该是服务端的原话 —— 两者不能互相顶替',
      );
    });
  });

  group('修改时间', () {
    testWidgets('有 mtime 就显示相对时间，没有就整行不画时间', (tester) async {
      final now = DateTime.now();
      final api = _TreeApi({
        kSandboxRoot: [
          FileNode(
            name: 'fresh.txt',
            path: '$kSandboxRoot/fresh.txt',
            isDirectory: false,
            sizeBytes: 12,
            modifiedAt: now.subtract(const Duration(seconds: 5)),
          ),
          FileNode(
            name: 'unknown.txt',
            path: '$kSandboxRoot/unknown.txt',
            isDirectory: false,
            sizeBytes: 12,
          ),
        ],
      });
      await tester.pumpWidget(_wrap(const SandboxBrowser(), api));
      await tester.pumpAndSettle();

      expect(
        find.text('刚刚'),
        findsOneWidget,
        reason:
            '「这是 agent 刚写的那个吗」是用户扫这棵树时最常问的问题，'
            '而名字和大小都回答不了它',
      );
      expect(
        find.textContaining('1970'),
        findsNothing,
        reason:
            'mtime 缺失时必须整个不画。回落成 epoch 会渲染出一个煞有介事的假日期，'
            '比留空误导得多 —— 用户没法把它和「真的是 1970 年」区分开',
      );
    });
  });

  group('文件树是懒加载的', () {
    testWidgets('第一次只要根这一层', (tester) async {
      final api = _TreeApi(_threeLevels());
      await tester.pumpWidget(_wrap(const SandboxBrowser(), api));
      await tester.pumpAndSettle();

      expect(
        api.listed,
        [kSandboxRoot],
        reason:
            '开面板只该打一个请求。急切递归的话，一个带 node_modules 的工作区'
            '会在这里打出上千个 —— 而用户第一眼要看的就是顶层那几个文件',
      );
      expect(find.text('src'), findsOneWidget);
      expect(
        find.text('main.py'),
        findsNothing,
        reason: 'src 还没展开，它里面的东西不该已经在树上 —— 那说明整棵树被拉下来了',
      );
    });

    testWidgets('展开一层只请求这一层，孙子那层原地不动', (tester) async {
      final api = _TreeApi(_threeLevels());
      await tester.pumpWidget(_wrap(const SandboxBrowser(), api));
      await tester.pumpAndSettle();

      await tester.tap(find.text('src'));
      await tester.pumpAndSettle();

      expect(api.listed, [
        kSandboxRoot,
        '$kSandboxRoot/src',
      ], reason: '展开 src 该且只该多出 src 这一条请求');
      expect(find.text('util'), findsOneWidget, reason: 'src 这一层的内容要出现');
      expect(
        api.listed,
        isNot(contains('$kSandboxRoot/src/util')),
        reason:
            'util 只是被**列出来**了，没被展开。这时候就去拉它的内容，'
            '等于把懒加载退化成了「晚一拍的急切加载」',
      );
      expect(
        find.text('io.py'),
        findsNothing,
        reason: 'util 里的文件不该在 util 展开之前就出现在树上',
      );

      await tester.tap(find.text('util'));
      await tester.pumpAndSettle();
      expect(api.listed, [
        kSandboxRoot,
        '$kSandboxRoot/src',
        '$kSandboxRoot/src/util',
      ], reason: '第三层要等到它自己被点开才拉');
      expect(find.text('io.py'), findsOneWidget);
    });

    testWidgets('折叠再展开不重新拉一遍', (tester) async {
      final api = _TreeApi(_threeLevels());
      await tester.pumpWidget(_wrap(const SandboxBrowser(), api));
      await tester.pumpAndSettle();

      await tester.tap(find.text('src'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('src')); // 折叠
      await tester.pumpAndSettle();
      await tester.tap(find.text('src')); // 再展开
      await tester.pumpAndSettle();

      expect(
        api.listed.where((p) => p == '$kSandboxRoot/src').length,
        1,
        reason:
            '每次展开都重拉一遍，等于把「已经看过的目录」也变成往返成本 —— '
            '用户在树上来回点几下就会明显感到卡',
      );
    });

    testWidgets('某一层列不出来时，只有那一层报错，其余的树照常在', (tester) async {
      // src 在根的列表里，但它自己那一层查不到 —— 模拟 agent 正好把它删了
      final api = _TreeApi({
        kSandboxRoot: [
          _dir('$kSandboxRoot/src'),
          _file('$kSandboxRoot/README.md', 12),
        ],
      });
      await tester.pumpWidget(_wrap(const SandboxBrowser(), api));
      await tester.pumpAndSettle();

      await tester.tap(find.text('src'));
      await tester.pumpAndSettle();

      expect(
        find.text('README.md'),
        findsOneWidget,
        reason: '一个子目录读不了，不该把整棵树换成一块错误提示',
      );
      expect(find.textContaining('不存在'), findsOneWidget);
    });
  });

  group('点文件就下载', () {
    testWidgets('取的是这个节点的绝对路径，失败时原样说原因', (tester) async {
      final api = _TreeApi(_threeLevels());
      await tester.pumpWidget(_wrap(const SandboxBrowser(), api));
      await tester.pumpAndSettle();

      await tester.tap(find.text('README.md'));
      await tester.pumpAndSettle();

      expect(
        api.read,
        ['$kSandboxRoot/README.md'],
        reason:
            '节点自带绝对路径就是为了这个：拿名字去拼、或者从祖先回溯，'
            '在树被折叠过之后就会指错文件',
      );
      expect(
        find.text('读不了'),
        findsOneWidget,
        reason: '下载失败的原因同样来自服务端，同样不该被换成一句通用的话',
      );
    });

    testWidgets('点目录不会去读文件', (tester) async {
      final api = _TreeApi(_threeLevels());
      await tester.pumpWidget(_wrap(const SandboxBrowser(), api));
      await tester.pumpAndSettle();

      await tester.tap(find.text('src'));
      await tester.pumpAndSettle();

      expect(api.read, isEmpty, reason: '目录只该展开。把目录当文件下载，服务端会回一堆没法解释的错');
    });
  });

  group('上传落点', () {
    testWidgets('默认是根，点开哪个目录就跟到哪个目录', (tester) async {
      final api = _TreeApi(_threeLevels());
      await tester.pumpWidget(_wrap(const SandboxBrowser(), api));
      await tester.pumpAndSettle();

      expect(
        find.text('传文件到 workspace/'),
        findsOneWidget,
        reason: '按钮上必须写清文件会落到哪儿 —— 「传进去了但不知道在哪」是这个入口最容易给出的坏体验',
      );

      await tester.tap(find.text('src'));
      await tester.pumpAndSettle();
      expect(
        find.text('传文件到 src/'),
        findsOneWidget,
        reason: '落点跟着用户刚点开的目录走，按钮上的字必须同步 —— 否则文件会静悄悄落到别处',
      );
    });
  });

  group('上传进度', () {
    test('一个文件时只报名字，多个文件时报第几个', () {
      expect(
        const SandboxUpload(done: 0, total: 1, name: 'data.csv').label,
        '正在传 data.csv',
        reason: '只有一个文件时「1/1」是一句废话，用户想知道的是卡在哪个文件上',
      );
      expect(
        const SandboxUpload(done: 2, total: 7, name: 'big.zip').label,
        '正在传 3/7：big.zip',
        reason:
            'done 是**已经传完**的个数，正在传的是第 done+1 个。'
            '直接显示 done 的话，传第一个文件时会写「正在传 0/7」',
      );
    });

    test('小文件不报百分比，大文件报，满了之后说「收尾中」', () {
      expect(
        const SandboxUpload(
          done: 0,
          total: 1,
          name: 'tiny.txt',
          sent: 900,
          bytes: 1000,
        ).label,
        '正在传 tiny.txt',
        reason:
            '分块是 64 KiB，比它小的文件只会有一次回调 —— '
            '闪一下「90%」只是噪声',
      );
      expect(
        const SandboxUpload(
          done: 0,
          total: 1,
          name: 'big.zip',
          sent: 300 * 1024,
          bytes: 1024 * 1024,
        ).label,
        '正在传 big.zip 29%',
        reason: '大文件上这个数就是「不是卡死了」的全部证据',
      );
      expect(
        const SandboxUpload(
          done: 0,
          total: 1,
          name: 'big.zip',
          sent: 1024 * 1024,
          bytes: 1024 * 1024,
        ).label,
        '正在传 big.zip 收尾中',
        reason:
            'Web 上进度会先冲到 100% 再干等（浏览器不支持流式请求体，'
            'FetchClient 先把流抽干再上传）。那时显示 100% 是在撒谎 —— '
            '真正的上传还一个字节都没走',
      );
    });
  });

  group('平台判据', () {
    testWidgets('有本地 agent 的构建里整块都不出现', (tester) async {
      final api = _TreeApi(_threeLevels());
      await tester.pumpWidget(_wrap(const SandboxWorkspaceView(), api));
      await tester.pumpAndSettle();

      expect(
        find.byType(SandboxBrowser),
        kLocalAgentSupported ? findsNothing : findsOneWidget,
        reason: kLocalAgentSupported
            ? '桌面端 agent 动的就是用户自己的目录。给它一个云沙箱文件树，'
                  '等于在本机文件旁边摆一份看不见你项目的副本'
            : 'Web 端没有它就既看不见也送不进去 —— 这个功能在产品上等于不存在',
      );
      expect(
        api.listed,
        kLocalAgentSupported ? isEmpty : isNotEmpty,
        reason: '不显示的时候也不该发请求 —— 那会在桌面端凭空拉起一个云容器',
      );
    });
  });

  group('Mock 夹具', () {
    test('列目录只给一层', () async {
      final api = MockCortexApi();
      final root = await api.sandboxListFiles(kSandboxRoot);
      final names = root.map((n) => n.name).toList();

      expect(
        names,
        containsAll(['out', 'src', 'README.md']),
        reason: '根这一层该有目录也有文件，界面上两种行才都画得出来',
      );
      expect(
        names,
        isNot(contains('report.md')),
        reason:
            'report.md 在 out/ 里面。夹具如果把嵌套内容摊平到根，'
            '界面上的懒加载就永远测不到 —— 而真后端不会这么给',
      );
      expect(root.first.isDirectory, isTrue, reason: '目录排在文件前面，与桌面端那棵树一致');
      expect(
        root.firstWhere((n) => n.isDirectory).sizeBytes,
        isNull,
        reason: '目录不该带字节数 —— 服务端给的 0 不是「空目录」的意思，画出来是句假话',
      );
    });

    test('写进去的文件，下一次列目录就看得见', () async {
      final api = MockCortexApi();
      final bytes = Uint8List.fromList(utf8.encode('hello'));
      final receipt = await api.sandboxWriteFile(
        path: '$kSandboxRoot/out/新的.txt',
        bytes: bytes,
      );

      expect(receipt.size, 5);
      final out = await api.sandboxListFiles('$kSandboxRoot/out');
      expect(
        out.map((n) => n.name),
        contains('新的.txt'),
        reason: '一个「收下了但列目录时看不到」的夹具，会让上传按钮在唯一能离线演示的模式里看起来是坏的',
      );
      expect(await api.sandboxReadFile('$kSandboxRoot/out/新的.txt'), bytes);
    });

    test('越界的路径按服务端那样拒掉', () async {
      final api = MockCortexApi();
      await expectLater(
        api.sandboxListFiles('/etc'),
        throwsA(
          isA<CortexApiException>().having(
            (e) => e.statusCode,
            'statusCode',
            400,
          ),
        ),
        reason: '夹具什么都收的话，越界那条分支就只会在生产上第一次被执行',
      );
    });
  });

  group('HTTP 客户端', () {
    HttpCortexApi apiServing(
      Future<http.Response> Function(http.Request) handler,
    ) => HttpCortexApi(
      baseUrl: 'https://cortex.example.com',
      token: 't',
      client: MockClient(handler),
    );

    test('entries 变成绝对路径，用的是服务端回的 path', () async {
      late Uri seen;
      final api = apiServing((req) async {
        seen = req.url;
        return http.Response.bytes(
          utf8.encode(
            jsonEncode({
              // 服务端把结尾那个斜杠规范化掉了
              'path': '$kSandboxRoot/out',
              'entries': [
                {'name': 'figures', 'is_dir': true, 'size': 0},
                {'name': 'report.md', 'is_dir': false, 'size': 123},
              ],
            }),
          ),
          200,
          headers: {'content-type': 'application/json'},
        );
      });

      final nodes = await api.sandboxListFiles('$kSandboxRoot/out/');
      expect(seen.path, '/sandbox/files');
      expect(seen.queryParameters['path'], '$kSandboxRoot/out/');
      expect(
        nodes.map((n) => n.path),
        ['$kSandboxRoot/out/figures', '$kSandboxRoot/out/report.md'],
        reason:
            '子节点的路径要拿服务端回的 path 去拼。拿请求里那份拼，'
            '这里就会拼出带双斜杠的 `/workspace/out//report.md` —— '
            '一条服务端没见过的字符串，下一次展开或下载就 404',
      );
      expect(nodes.first.sizeBytes, isNull, reason: '目录的 size 是占位的 0，不该显示');
      expect(nodes.last.sizeBytes, 123);
    });

    test('501 那句中文原样到达调用方，没有变成乱码', () async {
      final api = apiServing(
        (_) async => http.Response.bytes(
          utf8.encode(jsonEncode({'error': _kNoSandbox})),
          501,
          // axum 的 Json 就是这么发的：**没有 charset**。
          // `package:http` 的 Response.body 于是按 latin1 解，中文全废
          headers: {'content-type': 'application/json'},
        ),
      );

      await expectLater(
        api.sandboxListFiles(kSandboxRoot),
        throwsA(
          isA<CortexApiException>()
              .having((e) => e.message, 'message', _kNoSandbox)
              .having((e) => e.statusCode, 'statusCode', 501),
        ),
        reason:
            '这句话要一路活到界面上。走 Response.body（latin1）的话，'
            '用户看到的是「è¿™ä¸ªéƒ¨ç½²â€¦」—— 而这恰好是唯一一句他必须读懂的提示。'
            '外面那层 {"error": …} 也要剥掉，用户不该读到 JSON',
      );
    });

    test('写文件：路径在 query，字节在 body', () async {
      late http.Request seen;
      final api = apiServing((req) async {
        seen = req;
        return http.Response.bytes(
          utf8.encode(jsonEncode({'path': '$kSandboxRoot/a.txt', 'size': 5})),
          200,
          headers: {'content-type': 'application/json'},
        );
      });

      final receipt = await api.sandboxWriteFile(
        path: '$kSandboxRoot/a.txt',
        bytes: Uint8List.fromList([1, 2, 3, 4, 5]),
      );

      expect(seen.method, 'PUT');
      expect(seen.url.path, '/sandbox/files');
      expect(seen.url.queryParameters['path'], '$kSandboxRoot/a.txt');
      expect(seen.bodyBytes, [
        1,
        2,
        3,
        4,
        5,
      ], reason: '请求体就是文件本身。多包一层 JSON 或 base64 会让二进制文件在路上被改写');
      expect(receipt.path, '$kSandboxRoot/a.txt');
      expect(receipt.size, 5);
    });

    test('读文件走 raw 路由，回的是原始字节', () async {
      late Uri seen;
      final api = apiServing((req) async {
        seen = req.url;
        return http.Response.bytes(
          [0x89, 0x50, 0x4E, 0x47],
          200,
          headers: {'content-type': 'application/octet-stream'},
        );
      });

      final bytes = await api.sandboxReadFile('$kSandboxRoot/a.png');
      expect(seen.path, '/sandbox/files/raw');
      expect(seen.queryParameters['path'], '$kSandboxRoot/a.png');
      expect(bytes, [
        0x89,
        0x50,
        0x4E,
        0x47,
      ], reason: '字节不能经过任何字符串解码 —— 那会把 PNG 头改成替换字符');
    });
  });
}
