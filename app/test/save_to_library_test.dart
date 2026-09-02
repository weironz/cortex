/// **资料库要真的能进得去东西。**
///
/// 2026-09-02 用户实报：「生成的图片为什么不会显示在资料库里」。
///
/// 查下来不是「图没进去」，是**什么都进不去**：`addToLibrary()` 在接口、
/// HTTP 实现、mock 里各写了一遍，服务端 `POST /library` 连 `origin`
/// 的 `generated` 一档、以及「图片不切分」那一支都写好了 ——
/// 而客户端**零个调用点**。生产上跑了一周多，`library_items` 是 0 行，
/// 同一个库里 `generated_images` 有 8 行。
///
/// 更糟的是资料库那一屏的空态**答应了两件事**：「把文件拖进对话里发出去，
/// 它就会进资料库；画出来的图也可以从图片页收进来」—— 两件都没做。
/// 这是仓库里那个反复的形状「造好了没人调用」，加上「界面说了做不到的话」。
///
/// 这个文件一条守一处：图那条路、文件那条路、以及那句空态。
library;

import 'dart:async';

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/chat/widgets/attachment_views.dart';
import 'package:cortex_app/features/images/widgets/image_actions.dart';
import 'package:cortex_app/features/library/library_page.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/library_item.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/library_save.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

final _hash = 'ab12${'0' * 60}';

/// 记下每一次 `addToLibrary` 的参数。
class _Api extends MockCortexApi {
  _Api() : super(instant: true);

  final List<({String hash, String name, String? origin})> added = [];

  @override
  Future<LibraryItem> addToLibrary({
    required String blobHash,
    required String name,
    String? origin,
    String? folderId,
  }) async {
    added.add((hash: blobHash, name: name, origin: origin));
    return super.addToLibrary(
      blobHash: blobHash,
      name: name,
      origin: origin,
      folderId: folderId,
    );
  }
}

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

void main() {
  ProviderContainer boot(_Api api) {
    final c = ProviderContainer(
      overrides: [
        appConfigProvider.overrideWith(_MockConfig.new),
        cortexApiProvider.overrideWith((ref) => api),
      ],
    );
    addTearDown(c.dispose);
    return c;
  }

  group('图那条路', () {
    testWidgets('右键菜单里有「存进资料库」，点下去真的收进去了', (tester) async {
      final api = _Api();
      final c = boot(api);
      late WidgetRef captured;
      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: c,
          child: MaterialApp(
            home: Scaffold(
              body: Consumer(
                builder: (_, ref, _) {
                  captured = ref;
                  return const SizedBox();
                },
              ),
            ),
          ),
        ),
      );

      final said = <String>[];
      await ImageActions(
        ref: captured,
        hash: _hash,
        prompt: '一只戴眼镜的柯基',
        said: said.add,
      ).saveToLibrary();
      await tester.pumpAndSettle();

      expect(
        api.added,
        hasLength(1),
        reason:
            '「存进资料库」没有真的调 addToLibrary —— 那正是修之前的状态：'
            '接口写好了，零个调用点',
      );
      expect(
        api.added.single.origin,
        'generated',
        reason:
            'origin 没传 generated，资料库那一格会画成「已上传」。'
            '服务端专门为这一档留了值，不传等于那个徽章永远没人用得到',
      );
      expect(
        api.added.single.name,
        '一只戴眼镜的柯基',
        reason: '名字该用画它的那句话 —— 一屏缩略图里哈希谁也认不出来',
      );
      expect(
        said.single,
        contains('检索不到'),
        reason:
            '图片在服务端落的是 chunk_state = unsupported（没有正文可切）。'
            '不说清的话，用户会以为收进去模型就查得到，然后认定资料库坏了',
      );
    });

    /// **菜单项要真的画出来。** 上面那条直接调的是方法 ——
    /// 方法对了而菜单里没有这一项的话，用户仍然点不到它。
    testWidgets('菜单里那一项在，且不依赖「这张图在图库里」', (tester) async {
      final api = _Api();
      final c = boot(api);
      late BuildContext ctx;
      late WidgetRef captured;
      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: c,
          child: MaterialApp(
            home: Scaffold(
              body: Consumer(
                builder: (context, ref, _) {
                  ctx = context;
                  captured = ref;
                  return const SizedBox.expand();
                },
              ),
            ),
          ),
        ),
      );

      // ★ `lookupByHash: false` + 没有 galleryId ⇒ `inGallery == false`：
      // 对话里贴上来的那种图。分享 / 移除 / 归档三项都不该出现，
      // 而「存进资料库」**必须**出现 —— 它只要一个哈希
      unawaited(
        showImageContextMenu(
          ctx,
          const Offset(10, 10),
          ImageActions(ref: captured, hash: _hash, said: (_) {}),
        ),
      );
      await tester.pumpAndSettle();

      expect(
        find.text('存进资料库'),
        findsOneWidget,
        reason: '菜单里没有这一项 —— 方法写好了也没有入口，与修之前没有区别',
      );
      expect(
        find.text('从图库移除'),
        findsNothing,
        reason: '前提没成立：这张图被当成在图库里了，测不出「不依赖 inGallery」',
      );
    });
  });

  group('文件那条路', () {
    testWidgets('对话里的文件附件右键能收进资料库', (tester) async {
      final api = _Api();
      final c = boot(api);
      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: c,
          child: MaterialApp(
            home: Scaffold(
              body: AttachmentStrip(
                attachments: [
                  Attachment(
                    hash: _hash,
                    filename: '接口规范.md',
                    kind: 'document',
                    mime: 'text/markdown',
                    sizeBytes: 4096,
                  ),
                ],
              ),
            ),
          ),
        ),
      );
      await tester.pumpAndSettle();

      final card = find.text('接口规范.md');
      expect(card, findsOneWidget, reason: '前提没成立：这张卡片根本没画出来');

      await tester.tapAt(
        tester.getCenter(card),
        buttons: kSecondaryMouseButton,
      );
      await tester.pumpAndSettle();

      expect(
        find.text('存进资料库'),
        findsOneWidget,
        reason:
            '文件附件上没有这个入口 —— 于是用户**没有任何办法**把自己的'
            '文档放进资料库，而那正是这一屏存在的理由',
      );

      await tester.tap(find.text('存进资料库'));
      await tester.pumpAndSettle();

      expect(api.added, hasLength(1));
      expect(
        api.added.single.origin,
        isNull,
        reason: '自己的文档不该标成 generated —— 那一档是画出来的东西',
      );
      expect(api.added.single.name, '接口规范.md');
    });
  });

  /// **空态不许答应做不到的事。**
  ///
  /// 原文写着「把文件拖进对话里发出去，它就会进资料库」，而附件那条路
  /// 一行都没有碰过 library。用户照着做、什么都没发生，然后来问。
  testWidgets('空态说的是真的做得到的那条路', (tester) async {
    final api = _Api();
    final c = boot(api);
    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: c,
        child: const MaterialApp(home: Scaffold(body: LibraryPageView())),
      ),
    );
    await tester.pumpAndSettle();

    expect(
      find.textContaining('拖进对话里发出去'),
      findsNothing,
      reason:
          '空态还在答应「发出去就会进资料库」—— 那条路不存在，'
          '照做的人会以为是资料库坏了',
    );
    expect(
      find.textContaining('存进资料库'),
      findsWidgets,
      reason: '空态得说清真正的入口，否则它只是一句「还是空的」',
    );
  });

  group('名字怎么定', () {
    test('没有说明就退回哈希前八位', () {
      expect(libraryNameFor(hash: _hash), 'cortex-ab120000');
      expect(libraryNameFor(hash: _hash, label: '   '), 'cortex-ab120000');
    });

    test('按码点截断，不把一个字劈成半个', () {
      // 61 个四字节字符：按 UTF-16 单元截会正好劈在一对代理项中间，
      // 落库的是一个替换字符
      final long = '🐕' * 61;
      final name = libraryNameFor(hash: _hash, label: long);
      expect(
        name.runes.length,
        61,
        reason: '60 个字 + 一个省略号；按 UTF-16 截的话这里会是 31',
      );
      expect(name.contains('�'), isFalse, reason: '截在了一对代理项中间 —— 名字末尾会是一个替换字符');
    });
  });
}
