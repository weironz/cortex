/// **资料库是自动收的，界面上不该再有「存进资料库」那一步。**
///
/// 2026-09-02 的三次反复，这个文件守的是第三次的结论：
///
/// 1. 用户问「生成的图片为什么不会显示在资料库里」—— 查下来资料库**什么都
///    进不去**：`addToLibrary()` 在接口 / HTTP 实现 / mock 里各写了一遍，
///    零个调用点，生产上 `library_items` 0 行、`generated_images` 8 行。
/// 2. 先补了右键手动收。用户当场指出「不是多此一举吗，ChatGPT 和 Gemini
///    都是自动的吧」—— 他是对的。
/// 3. 改成**服务端自动收**（`POST /episodes` 上，见 `episodes.rs` 的
///    `collect_attachments`），客户端那两个手动入口一起撤掉。
///
/// 判据写在 `docs/library-content.md`：不判断「这是不是值得收的东西」——
/// 漏收补救不了（用户不知道去哪找，甚至不知道有东西没被存），膨胀补救得了
/// （搜索、配额、删除）。
///
/// # 为什么自动那一半不在这个文件里
///
/// 它在**服务端**：客户端做的话，从 CLI 发一个带附件的消息就不会进资料库，
/// 而 CLI 与 Flutter 走完全相同的 HTTP 协议（CLAUDE.md：不走私有捷径）。
/// 那一半由 `library::tests::唯一那个记录点在真库上的行为` 盯着。
///
/// 这里只守**客户端不许再长出第二条路**：手动入口一旦回来，同一份内容会有
/// 两个来源，而其中一个只有 Flutter 有。
library;

import 'dart:async';

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/theme.dart';
import 'package:cortex_app/features/chat/widgets/attachment_views.dart';
import 'package:cortex_app/features/images/widgets/image_actions.dart';
import 'package:cortex_app/features/library/library_page.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

final _hash = 'ab12${'0' * 60}';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

void main() {
  ProviderContainer boot() {
    final c = ProviderContainer(
      overrides: [
        appConfigProvider.overrideWith(_MockConfig.new),
        cortexApiProvider.overrideWith((ref) => MockCortexApi(instant: true)),
      ],
    );
    addTearDown(c.dispose);
    return c;
  }

  /// **空态说的必须是真的做得到的那条路。**
  ///
  /// 这句话答应过两次做不到的事（见文件头）。它是资料库唯一的说明书 ——
  /// 说错的代价是用户照着做、什么都没发生，然后认定功能坏了。
  testWidgets('空态说的是「自动收」，不再教人去右键', (tester) async {
    final c = boot();
    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: c,
        child: const MaterialApp(home: Scaffold(body: LibraryPageView())),
      ),
    );
    await tester.pumpAndSettle();

    expect(
      find.textContaining('自动'),
      findsWidgets,
      reason: '空态没说清材料是自动进来的 —— 用户会在这一页上找上传按钮，而没有',
    );
    expect(
      find.textContaining('拖进对话里发出去'),
      findsNothing,
      reason: '第一版那句话：附件发出去并不会进资料库（当时那条路根本没接）',
    );
    expect(
      find.textContaining('右键'),
      findsNothing,
      reason:
          '第二版那句话：手动入口已经撤了。留着的话用户会去找一个不存在的'
          '菜单项，而真正发生的事（自动收）反倒没人告诉他',
    );
  });

  group('客户端不许再长出第二条手动路', () {
    testWidgets('图片右键菜单里没有「存进资料库」', (tester) async {
      final c = boot();
      late BuildContext ctx;
      late WidgetRef captured;
      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: c,
          child: MaterialApp(
            theme: CortexTheme.light(),
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
        findsNothing,
        reason:
            '手动入口回来了。画出来的图在生成那一刻就以 generated 收进去了，'
            '这一项点下去只是幂等地回原来那条 —— 却说「已收进资料库」，'
            '像是刚做了什么',
      );
      // 正对照：菜单本身还在，不是被整个删掉了
      expect(
        find.text('另存为…'),
        findsOneWidget,
        reason: '前提没成立：菜单根本没弹出来，上一条断言是空的',
      );
    });

    testWidgets('文件附件上没有右键菜单', (tester) async {
      final c = boot();
      await tester.pumpWidget(
        UncontrolledProviderScope(
          container: c,
          child: const MaterialApp(
            home: Scaffold(
              body: AttachmentStrip(
                attachments: [
                  Attachment(
                    hash:
                        'cd34000000000000000000000000000000000000000000000000000000000000',
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
        findsNothing,
        reason:
            '手动入口回来了。发出去那一刻服务端已经收过了，这一项要么是'
            '空动作，要么就是在 Flutter 上多了一条 CLI 没有的路',
      );
    });
  });
}
