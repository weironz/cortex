/// 白纸 —— 点了「新对话」还没开口的那一屏。
///
/// # 为什么这一屏值得单独一组
///
/// 会话惰性化（`ChatController.startNewChat`）之后，「还没开口」有了两种
/// 长得不一样的实现：**没有会话**（白纸）与**有会话但一条消息都没有**。
/// 用户眼里它们是同一件事，而代码里差一个 `activeSessionId`。
///
/// 每一处按 `session == null` 分叉的界面，都可能只照顾了其中一种：
///
/// 1. **输入框的位置。** 判据里带 `hasSession` 的话，白纸走钉底那一支，
///    而会话在回车那一刻兑现 —— 用户看到的是「输入框跳到中间、又立刻
///    跳回底部」，一次发送闪两下。
/// 2. **工作区那颗 chip。** 它从前在 `session == null` 时整个不画，于是
///    新对话里没法先定「这次的文件落在哪」—— 而那正是最该在文件产生
///    **之前**说清的事。
/// 3. **兑现的时机。** 修好 2 之后紧跟着的坑是修过头：在**点开**清单那一刻
///    就把会话建出来。那样左栏立刻多一行谁也没说过话的「新会话」，
///    正是惰性化要消除的东西。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/chat/chat_pane.dart';
import 'package:cortex_app/features/chat/widgets/conversation_view.dart';
import 'package:cortex_app/features/workspace/workspace_panel.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

ProviderContainer _boot() => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_MockConfig.new),
    cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
    settingsReaderProvider.overrideWithValue(
      () async => const <String, String>{},
    ),
    settingsWriterProvider.overrideWithValue((_) async {}),
  ],
);

Future<void> _pump(
  WidgetTester tester,
  ProviderContainer c,
  Widget child,
) async {
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: c,
      // 够宽够高，免得被布局挤掉某个 chip 之后测出一个假的「不在」
      child: MaterialApp(
        home: Scaffold(body: SizedBox(width: 1000, height: 800, child: child)),
      ),
    ),
  );
  await tester.pump(const Duration(milliseconds: 100));
}

void main() {
  testWidgets('白纸上输入框就在中间，不是先钉底再跳过去', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);
    // 白纸 = 没有当前会话
    c.read(chatControllerProvider.notifier).startNewChat();

    await _pump(tester, c, const ChatPane());

    expect(
      find.byType(ConversationHero),
      findsOneWidget,
      reason:
          '居中那一支带着招呼与起手式。判据里多一个 `hasSession` 的话，'
          '白纸会走钉底那一支 —— 回车之后会话兑现，界面先跳到居中再跳回底部，'
          '一次发送闪两下，而中间那一帧还是空的',
    );
    expect(
      find.byType(ConversationPrompts),
      findsOneWidget,
      reason: '起手式与居中形态是同一支，它不在就说明走错了分支',
    );
  });

  testWidgets('白纸上就能先定工作区', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);
    c.read(chatControllerProvider.notifier).startNewChat();

    await _pump(tester, c, const WorkspaceChip());

    expect(
      find.text('新建工作区'),
      findsOneWidget,
      reason:
          '「这次的文件落在哪」必须在文件产生**之前**说得出。这颗 chip 从前在'
          '没有会话时整个不画，于是新对话里只能先随便说一句话把会话催出来，'
          '再回头改 —— 而那时文件已经落在别处了',
    );
  });

  testWidgets('⚠️ 点开工作区清单又划掉，不留下一条空会话', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);
    c.read(chatControllerProvider.notifier).startNewChat();

    await _pump(tester, c, const WorkspaceChip());
    // ⚠️ 基线在 pump **之后**取。开机那次会话列表是异步回来的，pump 之前
    // 读到的是还没回来时的 0，而断言执行时 mock 那几条已经到了 ——
    // 测试会红在「多了四条」上，与被测的行为毫无关系。
    // （不能像纯 `test()` 那样用 `Future.delayed` 轮询等它：`testWidgets`
    // 跑在 fake async 里，那个 delay 不 pump 就永远不完成，整条测试挂死。）
    final before = c.read(chatControllerProvider).sessions.length;
    await tester.tap(find.text('新建工作区'));
    await tester.pumpAndSettle();
    // 清单开出来了才谈得上「划掉它」
    expect(find.text('派出去跑'), findsOneWidget);

    // 划掉 = 什么都没选
    Navigator.of(tester.element(find.text('派出去跑'))).pop();
    await tester.pumpAndSettle();

    expect(
      c.read(chatControllerProvider).sessions.length,
      before,
      reason:
          '在**点开**清单那一刻就把会话建出来，等于凭一次「我看看有哪些目录」'
          '在左栏留下一行谁也没说过话的新会话 —— 那正是惰性化要消除的东西。'
          '兑现的时机是选完，不是点开',
    );
    expect(c.read(chatControllerProvider).activeSessionId, isNull);
  });

  testWidgets('白纸上选了「新建工作区」也不建会话 —— 它选的就是默认行为', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);
    c.read(chatControllerProvider.notifier).startNewChat();

    await _pump(tester, c, const WorkspaceChip());
    // ⚠️ 基线在 pump **之后**取。开机那次会话列表是异步回来的，pump 之前
    // 读到的是还没回来时的 0，而断言执行时 mock 那几条已经到了 ——
    // 测试会红在「多了四条」上，与被测的行为毫无关系。
    // （不能像纯 `test()` 那样用 `Future.delayed` 轮询等它：`testWidgets`
    // 跑在 fake async 里，那个 delay 不 pump 就永远不完成，整条测试挂死。）
    final before = c.read(chatControllerProvider).sessions.length;
    await tester.tap(find.text('新建工作区'));
    await tester.pumpAndSettle();
    // 清单里那一条同名项
    await tester.tap(find.text('新建工作区').last);
    await tester.pumpAndSettle();

    expect(
      c.read(chatControllerProvider).sessions.length,
      before,
      reason: '「新建工作区」= 首轮自己开一个，本来就是白纸的默认行为。为一次空操作建会话是白留一行',
    );
  });

  /// **点一下对话区，接着打字要能打进输入框。**
  ///
  /// # 这条为什么在这一组
  ///
  /// 起因是右栏那个真终端：它一旦拿到键盘焦点就一直拿着，而对话区里
  /// **没有任何可聚焦的东西**（一堆文字与气泡），点它不会把焦点摘走。
  /// 用户在终端里敲完命令、回来点一下对话、接着打字 —— 字全进了终端，
  /// 而屏幕上没有任何东西提示焦点在哪。
  ///
  /// 用一个原本没焦点的节点冒充「别处」：判据是「焦点不在这一栏里就要回来」，
  /// 谁拿着它并不重要。
  testWidgets('焦点在别处时，点一下对话区就把键盘交给输入框', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);
    c.read(chatControllerProvider.notifier).startNewChat();

    final elsewhere = FocusNode(debugLabel: '别处');
    addTearDown(elsewhere.dispose);
    await _pump(
      tester,
      c,
      Column(
        children: [
          // 「右栏那个终端」的替身
          Focus(focusNode: elsewhere, child: const SizedBox(height: 20)),
          const Expanded(child: ChatPane()),
        ],
      ),
    );

    elsewhere.requestFocus();
    await tester.pump();
    expect(elsewhere.hasPrimaryFocus, isTrue, reason: '前提没成立，后面测的就不是这件事');

    // 按坐标点，不按部件点：招呼那块本身不吃指针事件（它在滚动视图里、
    // 没有自己的命中区），`tap(find...)` 会警告「点在了别的东西上」。
    // 而这条测试要的正是「点在对话区的任意一处」，坐标就够了。
    await tester.tapAt(tester.getCenter(find.byType(ConversationHero)));
    await tester.pump();

    expect(
      elsewhere.hasPrimaryFocus,
      isFalse,
      reason:
          '点了对话区，焦点还留在别处 —— 接着敲的字会进那个「别处」，'
          '而现实里那个别处是右栏的终端',
    );
    expect(
      tester
          .widget<TextField>(find.byType(TextField).first)
          .focusNode
          ?.hasPrimaryFocus,
      isTrue,
      reason: '焦点该落到输入框上，而不是随便摘掉了事 —— 摘掉的话字哪儿都不去',
    );
  });

  /// ⚠️ **反方向：焦点已经在这一栏里时不许动它。**
  ///
  /// 少了这道判断，每一次在气泡上按下指针都会先把焦点抢给输入框。消息正文
  /// 是可选中的（`SelectionArea`），而选中区在失去焦点时会清掉选中 ——
  /// 于是「按住拖选一段话」变成选不动，而这个 bug 只有真去拖才看得见。
  ///
  /// ⚠️ 对照物必须是**输入框之外**的某个栏内节点。第一版拿输入框自己当
  /// 对照，结果那条测试是空的：对一个已经拿着焦点的节点再 `requestFocus`
  /// 什么都不会发生，把判断整条删掉照样绿。这里用真的消息气泡 ——
  /// 它也正是这道判断要保护的东西。
  testWidgets('焦点在栏内别的东西上时，点击不把它抢走', (tester) async {
    final c = _boot();
    addTearDown(c.dispose);

    await _pump(tester, c, const ChatPane());
    // 等 mock 那条会话的正文回来 —— 没有气泡就没有对照物
    for (var i = 0; i < 12; i++) {
      await tester.pump(const Duration(milliseconds: 100));
    }
    final bubbles = find.byType(SelectableRegion);
    expect(bubbles, findsWidgets, reason: '这条用例要的是真气泡，没有就说明前提没成立');

    final composer = tester
        .widget<TextField>(find.byType(TextField).first)
        .focusNode!;

    // 点一下消息正文：选中区拿到焦点（用户拖选的第一下就是这个）
    await tester.tapAt(tester.getCenter(bubbles.first));
    await tester.pump();
    final holder = FocusManager.instance.primaryFocus;
    expect(
      holder,
      isNot(anyOf(isNull, same(composer))),
      reason: '前提没成立：焦点该落在选中区上，后面测的才是「栏内别的东西」',
    );

    var stolen = 0;
    composer.addListener(() => stolen++);

    // 再点一下 —— 这一下就是「拖选的起手」
    await tester.tapAt(tester.getCenter(bubbles.first));
    await tester.pump();

    expect(
      stolen,
      0,
      reason:
          '焦点本来就在栏内（选中区）上，这一下把它抢给了输入框 —— '
          '选中区一失焦就清掉选中，表现是在消息上按住拖不动',
    );
  });
}
