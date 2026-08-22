/// 图库那一面：相册、多选，以及一张图上能做的那几件事。
///
/// # 这一组在盯的四件事
///
/// 1. **按哈希查图库那一行时，回来的必须自己核对哈希。** 老服务端不认得
///    `hash=`，会静默忽略它、回最新那一页 —— 不核对的话，「复制链接」
///    分享出去的是**别人的图**，而两边都不报错。
/// 2. **换相册要重取第一页，而不是过滤手上这些。** 过滤出来的那一份看着
///    是完整的，实际少掉所有还没翻到的。
/// 3. **勾中之后那一条整个换成动作条**，且「移出相册」只在真的看着某个
///    相册时出现 —— 在「全部」里它答不上来「从哪个里拿」。
/// 4. **空相册与空图库说的不是同一句话。** 在一个空相册里看到
///    「还没有画过图」，用户会以为自己的图全没了。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/chat_message.dart';
import 'package:cortex_app/models/image_prefs.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/chat/widgets/attachment_views.dart';
import 'package:cortex_app/features/images/image_page.dart';
import 'package:cortex_app/features/images/widgets/image_actions.dart';
import 'package:cortex_app/features/images/widgets/image_thumb.dart';
import 'package:cortex_app/features/images/widgets/image_viewer.dart';
import 'package:cortex_app/models/generated_image.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/image_controller.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

final _hashA = 'aa11${'0' * 60}';
final _hashB = 'bb22${'0' * 60}';

GeneratedImage _img(String id, String hash, {String prompt = '一只柴犬'}) =>
    GeneratedImage(
      id: id,
      hash: hash,
      prompt: prompt,
      model: 'gpt-image-2',
      source: 'src',
    );

/// 记下每一次 `gallery` 的参数，并按脚本回内容。
class _GalleryApi extends MockCortexApi {
  _GalleryApi({this.byAlbum = const {}, this.all = const []})
    : super(instant: true);

  final Map<String, List<GeneratedImage>> byAlbum;
  final List<GeneratedImage> all;

  final List<({String? album, String? hash, String? before})> calls = [];

  /// 装成一个**不认得 `hash=`** 的老服务端：忽略它，照回最新那一页。
  bool ignoreHash = false;

  @override
  Future<Gallery> gallery({
    int limit = 30,
    String? before,
    String? album,
    String? hash,
  }) async {
    calls.add((album: album, hash: hash, before: before));
    var items = album == null ? all : (byAlbum[album] ?? const []);
    if (hash != null && !ignoreHash) {
      items = items.where((i) => i.hash == hash).toList();
    }
    return Gallery(items: items.take(limit).toList());
  }

  @override
  Future<Albums> albums() async => Albums(
    albums: [
      for (final e in byAlbum.entries)
        Album(id: e.key, name: '相册 ${e.key}', count: e.value.length),
    ],
  );

  final List<String> shared = [];

  @override
  Future<String> shareImage(String id) async {
    shared.add(id);
    return 'https://example.invalid/s/tok/$id.png';
  }

  @override
  Future<Uint8List> blobBytes(String hash) async => Uint8List.fromList(const [
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, //
    0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
    0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41,
    0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
    0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
  ]);
}

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

ProviderContainer _boot(MockCortexApi api) => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_MockConfig.new),
    cortexApiProvider.overrideWithValue(api),
    settingsReaderProvider.overrideWithValue(
      () async => const <String, String>{},
    ),
    settingsWriterProvider.overrideWithValue((_) async {}),
  ],
);

Future<void> _pump(WidgetTester tester, ProviderContainer c) async {
  tester.view.physicalSize = const Size(1200, 1500);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: c,
      child: const MaterialApp(home: Scaffold(body: ImagePage())),
    ),
  );
  for (var i = 0; i < 10; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
  // 与 `image_page_test.dart` 逐字相同的入口：等会话列表落地再进图片页，
  // 否则那条草稿会被自动选中的第一条远端会话顶掉
  c.read(mainViewProvider.notifier).go(MainView.images);
  for (var i = 0; i < 8; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

Future<void> _settle(WidgetTester tester, [int rounds = 12]) async {
  for (var i = 0; i < rounds; i++) {
    await tester.pump(const Duration(milliseconds: 120));
  }
}

void main() {
  // ⚠️ **剪贴板那条平台通道必须替掉。**
  //
  // 「复制链接」最后一步是 `Clipboard.setData`。测试里没有 handler 时那个
  // Future 永远不完成 —— 表现是这条测试 `did not complete`，**并把同一个
  // 文件里后面所有测试一起拖死**（它们连跑都没跑就被判为未完成）。
  // 排起来极费劲：失败信息指向的是无辜的那几条。
  final clipboard = <String>[];
  setUp(() {
    clipboard.clear();
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(SystemChannels.platform, (call) async {
          if (call.method == 'Clipboard.setData') {
            clipboard.add((call.arguments as Map)['text'] as String);
          }
          return null;
        });
  });
  tearDown(
    () => TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(SystemChannels.platform, null),
  );

  group('按哈希找回图库那一行', () {
    testWidgets('回来的那一行哈希对不上就当作查不到', (tester) async {
      final api = _GalleryApi(all: [_img('IMG-A', _hashA)])
        // 老服务端：忽略 `hash=`，照回最新那一页
        ..ignoreHash = true;
      final c = _boot(api);
      addTearDown(c.dispose);

      final said = <String>[];
      late WidgetRef captured;
      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: c,
          child: MaterialApp(
            home: Consumer(
              builder: (_, ref, _) {
                captured = ref;
                return const SizedBox();
              },
            ),
          ),
        ),
      );

      // 手上这张的哈希是 B，而服务端回的是 A 那一行
      await ImageActions(
        ref: captured,
        hash: _hashB,
        lookupByHash: true,
        said: said.add,
      ).copyLink();

      expect(
        api.shared,
        isEmpty,
        reason:
            '拿一个碰巧排在前面的 id 去分享，用户会把**别人的图**发到公网上，'
            '而这一步两边都不报错',
      );
      expect(said.single, contains('不在图库里'));
    });

    testWidgets('哈希对得上就用那一行的 id 去分享', (tester) async {
      final api = _GalleryApi(all: [_img('IMG-B', _hashB)]);
      final c = _boot(api);
      addTearDown(c.dispose);

      final said = <String>[];
      late WidgetRef captured;
      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: c,
          child: MaterialApp(
            home: Consumer(
              builder: (_, ref, _) {
                captured = ref;
                return const SizedBox();
              },
            ),
          ),
        ),
      );

      await ImageActions(
        ref: captured,
        hash: _hashB,
        lookupByHash: true,
        said: said.add,
      ).copyLink();

      expect(api.shared, ['IMG-B']);
      expect(
        clipboard.single,
        contains('/s/'),
        reason: '「复制链接」得真的把那条链接放进剪贴板，光弹一句提示不算',
      );
      expect(
        said.single,
        contains('公开链接'),
        reason: '这是一条公开链接。不说清的话，用户以为自己只是「复制了个地址」',
      );
    });

    testWidgets('没开 lookupByHash 时一次都不问', (tester) async {
      final api = _GalleryApi(all: [_img('IMG-B', _hashB)]);
      final c = _boot(api);
      addTearDown(c.dispose);

      final said = <String>[];
      late WidgetRef captured;
      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: c,
          child: MaterialApp(
            home: Consumer(
              builder: (_, ref, _) {
                captured = ref;
                return const SizedBox();
              },
            ),
          ),
        ),
      );

      final plain = ImageActions(ref: captured, hash: _hashB, said: said.add);
      // `inGallery` 是菜单显不显那几项的判据
      expect(plain.inGallery, isFalse);
      expect(
        ImageActions(
          ref: captured,
          hash: _hashB,
          lookupByHash: true,
          said: said.add,
        ).inGallery,
        isTrue,
      );

      await plain.copyLink();
      expect(api.calls, isEmpty, reason: '对话之外那些图（用户自己传的）不该为了几个菜单项各发一个请求');
      expect(api.shared, isEmpty);
    });
  });

  group('相册', () {
    test('换相册是重取，不是过滤手上这些', () async {
      final api = _GalleryApi(
        all: [_img('IMG-1', _hashA), _img('IMG-2', _hashB)],
        byAlbum: {
          'alb-1': [_img('IMG-2', _hashB)],
        },
      );
      final c = _boot(api);
      addTearDown(c.dispose);

      final n = c.read(imageControllerProvider.notifier);
      await Future<void>.delayed(Duration.zero);
      await n.refresh();
      expect(c.read(imageControllerProvider).items, hasLength(2));

      await n.setAlbum('alb-1');
      expect(
        api.calls.last.album,
        'alb-1',
        reason:
            '只过滤手上这一页的话，「这个相册」会少掉所有还没翻到的 —— '
            '而它看起来是完整的',
      );
      expect(c.read(imageControllerProvider).items, hasLength(1));
      expect(c.read(imageControllerProvider).album, 'alb-1');
    });

    testWidgets('空相册说的不是「还没有画过图」', (tester) async {
      final api = _GalleryApi(all: const [], byAlbum: {'alb-1': const []});
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);

      await c.read(imageControllerProvider.notifier).setAlbum('alb-1');
      await _settle(tester);

      expect(find.text('这个相册还是空的'), findsOneWidget);
      expect(
        find.text('还没有画过图'),
        findsNothing,
        reason: '在一个空相册里看到这句，用户会以为自己的图全没了',
      );
    });
  });

  group('多选', () {
    testWidgets('勾中之后那一条换成动作条', (tester) async {
      final api = _GalleryApi(all: [_img('IMG-1', _hashA)]);
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);
      await _settle(tester);

      expect(find.byKey(const ValueKey('album:new')), findsOneWidget);
      expect(find.byKey(const ValueKey('sel:count')), findsNothing);

      c.read(imageControllerProvider.notifier).toggleSelect('IMG-1');
      await _settle(tester);

      expect(find.byKey(const ValueKey('sel:count')), findsOneWidget);
      expect(find.text('选中 1 张'), findsOneWidget);
      expect(
        find.byKey(const ValueKey('album:new')),
        findsNothing,
        reason: '两排按钮叠着的话，用户得先分清哪一排管的是哪件事',
      );
    });

    testWidgets('「移出相册」只在真的看着某个相册时出现', (tester) async {
      final api = _GalleryApi(
        all: [_img('IMG-1', _hashA)],
        byAlbum: {
          'alb-1': [_img('IMG-1', _hashA)],
        },
      );
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);
      await _settle(tester);

      c.read(imageControllerProvider.notifier).toggleSelect('IMG-1');
      await _settle(tester);
      expect(
        find.byKey(const ValueKey('sel:pull')),
        findsNothing,
        reason: '在「全部」里，「移出相册」答不上来「从哪个里拿」',
      );

      await c.read(imageControllerProvider.notifier).setAlbum('alb-1');
      await _settle(tester);
      c.read(imageControllerProvider.notifier).toggleSelect('IMG-1');
      await _settle(tester);
      expect(find.byKey(const ValueKey('sel:pull')), findsOneWidget);
    });

    test('换相册时勾选跟着作废', () async {
      final api = _GalleryApi(
        all: [_img('IMG-1', _hashA)],
        byAlbum: {'alb-1': const []},
      );
      final c = _boot(api);
      addTearDown(c.dispose);
      final n = c.read(imageControllerProvider.notifier);
      await n.refresh();
      n.toggleSelect('IMG-1');
      expect(c.read(imageControllerProvider).selected, {'IMG-1'});

      await n.setAlbum('alb-1');
      expect(
        c.read(imageControllerProvider).selected,
        isEmpty,
        reason: '留着的话，用户切到别的相册再点「加入相册」，加进去的是他现在根本看不见的那几张',
      );
    });
  });

  group('放大之后那一排', () {
    testWidgets('转过之后要说清导出的是原图', (tester) async {
      final c = _boot(_GalleryApi(all: [_img('IMG-1', _hashA)]));
      addTearDown(c.dispose);
      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: c,
          child: MaterialApp(
            home: Scaffold(
              body: ImageViewer(
                image: ViewerImage(hash: _hashA, galleryId: 'IMG-1'),
              ),
            ),
          ),
        ),
      );
      await _settle(tester, 4);

      expect(
        find.byKey(const ValueKey('viewer:preview-only')),
        findsNothing,
        reason: '没转过就别说这句 —— 一条常年挂着的提示等于没有提示',
      );

      await tester.tap(find.byKey(const ValueKey('viewer:rotr')));
      await _settle(tester, 4);

      expect(
        find.byKey(const ValueKey('viewer:preview-only')),
        findsOneWidget,
        reason:
            '⚠️ 旋转不改字节。不说的话，用户转正再另存得到的还是歪的 —— '
            '而他会以为是保存坏了',
      );

      // 复原之后那句话跟着收回去（它描述的是当下的状态，不是历史）
      await tester.tap(find.byTooltip('复原'));
      await _settle(tester, 4);
      expect(find.byKey(const ValueKey('viewer:preview-only')), findsNothing);
    });

    testWidgets('翻转也算「变换过」', (tester) async {
      final c = _boot(_GalleryApi(all: [_img('IMG-1', _hashA)]));
      addTearDown(c.dispose);
      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: c,
          child: MaterialApp(
            home: Scaffold(
              body: ImageViewer(image: ViewerImage(hash: _hashA)),
            ),
          ),
        ),
      );
      await _settle(tester, 4);

      await tester.tap(find.byKey(const ValueKey('viewer:fliph')));
      await _settle(tester, 4);
      expect(find.byKey(const ValueKey('viewer:preview-only')), findsOneWidget);
    });

    testWidgets('不知道提示词就不给「重画」那个按钮', (tester) async {
      final c = _boot(_GalleryApi(all: const []));
      addTearDown(c.dispose);
      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: c,
          child: MaterialApp(
            home: Scaffold(
              // 对话里那张图是附件，没有提示词
              body: ImageViewer(
                image: ViewerImage(hash: _hashA, lookupByHash: true),
              ),
            ),
          ),
        ),
      );
      await _settle(tester, 4);

      expect(
        find.byKey(const ValueKey('viewer:reprompt')),
        findsNothing,
        reason: '一个按下去什么都不发生的按钮，比没有这个按钮更让人费解',
      );
      // 但分享那条路是**开着**的：它按哈希去问图库那一行
      expect(find.byKey(const ValueKey('viewer:link')), findsOneWidget);
    });
  });

  group('画出来的图当场就要出现在对话里', () {
    test('终帧带的附件落到那条 assistant 消息上', () async {
      final api = _ScriptedChat();
      final c = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_MockConfig.new),
          cortexApiProvider.overrideWithValue(api),
        ],
      );
      addTearDown(c.dispose);
      c.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);

      final n = c.read(chatControllerProvider.notifier);
      // **拿住这条会话的 id。** 假后端里预置了别的会话，直接读
      // `activeTranscript.last` 会读到那些预置内容
      final sid = n.createSession();
      await n.send('画一只柴犬');
      // 流是同步喂完的，收尾要走几轮微任务
      for (var i = 0; i < 40; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 20));
        if (c.read(chatControllerProvider).streaming == null) break;
      }

      final last = c
          .read(chatControllerProvider)
          .transcripts[sid]!
          .messages
          .last;
      expect(last.role, MessageRole.assistant);
      expect(
        last.attachments.map((a) => a.hash),
        [_hashA],
        reason:
            '⚠️ 不带的话，用户看到模型说「画好啦」而屏幕上一张图都没有 —— '
            '要换个会话再回来（或者重启）才出现。2026-08-23 实测',
      );
      expect(last.attachments.single.isImage, isTrue);
    });

    test('老服务端不发这个字段时不炸，只是没有附件', () async {
      final api = _ScriptedChat(withAttachments: false);
      final c = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_MockConfig.new),
          cortexApiProvider.overrideWithValue(api),
        ],
      );
      addTearDown(c.dispose);
      c.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);

      final n = c.read(chatControllerProvider.notifier);
      // **拿住这条会话的 id。** 假后端里预置了别的会话，直接读
      // `activeTranscript.last` 会读到那些预置内容
      final sid = n.createSession();
      await n.send('画一只柴犬');
      for (var i = 0; i < 40; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 20));
        if (c.read(chatControllerProvider).streaming == null) break;
      }

      final last = c
          .read(chatControllerProvider)
          .transcripts[sid]!
          .messages
          .last;
      expect(last.text, contains('画好啦'));
      expect(last.attachments, isEmpty);
    });
  });

  group('线协议', () {
    test('done 的 attachments 解得出来', () {
      final e = ChatEvent.fromJson({
        'type': 'done',
        'episode_id': 'ep1',
        'attachments': [
          {'hash': _hashA, 'kind': 'image', 'filename': '生成的图.png'},
        ],
      });
      expect(e, isA<ChatDoneEvent>());
      final done = e as ChatDoneEvent;
      expect(done.attachments.single.hash, _hashA);
      expect(done.attachments.single.isImage, isTrue);
    });

    test('老服务端不带这个字段时是空表，不是异常', () {
      final done =
          ChatEvent.fromJson({'type': 'done', 'episode_id': 'ep1'})
              as ChatDoneEvent;
      expect(done.attachments, isEmpty);
    });
  });
  group('对话里那张图', () {
    testWidgets('右键出得来菜单，点下去真的会做事', (tester) async {
      final api = _GalleryApi(all: [_img('IMG-A', _hashA)]);
      final c = _boot(api);
      addTearDown(c.dispose);

      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: c,
          child: MaterialApp(
            home: Scaffold(
              body: Center(
                child: AttachmentStrip(
                  attachments: [
                    Attachment(hash: _hashA, kind: 'image', filename: '图.png'),
                  ],
                ),
              ),
            ),
          ),
        ),
      );
      await _settle(tester, 6);

      // 右键：与桌面上那一下同一条路（`onSecondaryTapDown`）
      final img = find.byType(AttachmentStrip);
      final at = tester.getCenter(img);
      final gesture = await tester.startGesture(at, buttons: kSecondaryButton);
      await gesture.up();
      await _settle(tester, 6);

      expect(find.text('复制图片链接'), findsOneWidget);

      await tester.tap(find.text('复制图片链接'));
      await _settle(tester, 8);

      expect(
        find.text('复制图片链接'),
        findsNothing,
        reason: '点了之后菜单要收起来 —— 收不起来的表现是整个界面像卡死了',
      );
      expect(
        api.shared,
        ['IMG-A'],
        reason:
            '对话里那张图只有哈希，要按哈希找回图库那一行才分享得了 —— '
            '找不回来的表现是同一张图在对话里能做的事比图库里少',
      );
    });
  });

  group('链接能走多远，要如实说', () {
    test('回环地址上不许说「任何人都能打开」', () {
      final said = reachOnly('http://127.0.0.1:5173/s/tok/image.png');
      expect(
        said,
        contains('只有这台机器'),
        reason:
            '「任何拿到它的人都能打开」在回环地址上是假的 —— 用户发给同事，'
            '对方看到的是连接被拒，而他会以为是分享坏了',
      );
      expect(reachOnly('http://localhost:8080/s/tok/image.png'), isNotEmpty);
    });

    test('内网地址也要提一句', () {
      expect(reachOnly('http://192.168.1.9/s/tok/image.png'), contains('内网'));
      expect(reachOnly('http://10.0.0.5/s/tok/image.png'), contains('内网'));
      expect(reachOnly('http://172.20.3.4/s/tok/image.png'), contains('内网'));
      // 172.32 不在 172.16–31 这一段里，是公网
      expect(reachOnly('http://172.32.3.4/s/tok/image.png'), isEmpty);
    });

    test('真域名上什么都不多说', () {
      expect(
        reachOnly('https://cortex.example.com/api/s/tok/image.png'),
        isEmpty,
        reason: '一条常年挂着的提示等于没有提示',
      );
    });
  });

  group('图库要有一个常驻入口', () {
    testWidgets('开口说过话之后，图库还找得回来', (tester) async {
      final api = _GalleryApi(all: [_img('IMG-1', _hashA)]);
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);
      await _settle(tester);

      // 落地页上图库本来就在
      expect(find.text('我的图片'), findsWidgets);

      // 说一句话 —— 图库让位给对话
      final n = c.read(chatControllerProvider.notifier);
      await n.send('画一只柴犬');
      await _settle(tester, 20);
      expect(find.byType(ImageThumb), findsNothing);

      // ⚠️ 这里就是用户报的那一句「图片库怎么找不到了」：
      // 从前唯一的回去办法是「新对话」，而没人会把「新建」读成「回到图库」
      final entry = find.byKey(const ValueKey('images:gallery'));
      expect(entry, findsOneWidget, reason: '图库是一个地方，不该藏在「新对话」后面');
      await tester.tap(entry);
      await _settle(tester, 20);

      expect(find.byType(ImageThumb), findsWidgets);
    });

    testWidgets('「新对话」要把人带回对话，不留在图库上', (tester) async {
      final api = _GalleryApi(all: [_img('IMG-1', _hashA)]);
      final c = _boot(api);
      addTearDown(c.dispose);
      await _pump(tester, c);
      await _settle(tester);

      await tester.tap(find.byKey(const ValueKey('images:gallery')));
      await _settle(tester, 12);
      expect(find.text('回到对话'), findsOneWidget);

      await tester.tap(find.byKey(const ValueKey('images:new')));
      await _settle(tester, 20);
      expect(
        find.text('我的图片'),
        findsWidgets,
        reason: '回到落地页 —— 停在图库上的话，「新对话」点了像没反应',
      );
      expect(find.text('回到对话'), findsNothing);
    });
  });
}

/// 一轮按脚本跑完：一句话 + 一次 `generate_image` + 终帧。
class _ScriptedChat extends MockCortexApi {
  _ScriptedChat({this.withAttachments = true}) : super(instant: true);

  /// `false` = 装成一个**不发 `attachments`** 的老服务端。
  final bool withAttachments;

  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
    PermissionMode permissionMode = PermissionMode.ask,
    String? model,
    String? source,
    ImagePrefs? imagePrefs,
  }) async* {
    yield const ChatToolEvent(
      name: 'generate_image',
      summary: '调用 generate_image',
      phase: ToolPhase.call,
    );
    yield const ChatToolEvent(
      name: 'generate_image',
      summary: 'generate_image 返回 2 行',
    );
    yield const ChatDeltaEvent('画好啦！');
    yield ChatDoneEvent(
      'ep1',
      attachments: withAttachments
          ? [Attachment(hash: _hashA, kind: 'image', filename: '生成的图.png')]
          : const [],
    );
  }
}
