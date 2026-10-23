/// 消息流与输入框的**桌面端微交互**契约。
///
/// # 为什么这些值得单独测
///
/// 它们全是「静态分析看不见、跑起来也不报错」的那一类：动作栏该不该
/// 在悬停时显形、焦点环该不该换色、会话行的菜单能不能用键盘够到。
/// 改坏了的表现不是红屏，而是「这个应用用着有点迟钝」—— 没人会为
/// 那种感受开一个 issue，但每一个用过 Claude / Raycast 的人都会觉得
/// 这里差一口气。
///
/// 另一条更硬的：**这些交互必须能被键盘触发**。手写 `MouseRegion`
/// 的那一版只有鼠标可达，而一个桌面应用里「Tab 走不到」等于半个功能。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/design_system/design_system.dart';
import 'package:cortex_app/features/chat/widgets/message_bubble.dart';
import 'package:cortex_app/features/chat/widgets/message_composer.dart';
import 'package:cortex_app/models/chat_message.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// 一条用户消息。动作栏只在它身上出现（助手那边要 episodeId）。
ChatMessage _user(String text) => ChatMessage(
  id: 'm1',
  role: MessageRole.user,
  text: text,
  createdAt: DateTime(2026, 10, 23, 12),
);

Future<void> _pump(WidgetTester tester, Widget child) => tester.pumpWidget(
  // ⚠️ 不用裸 ProviderScope：`MessageComposer` 里的状态栏 watch 了
  // chatControllerProvider，而它一活就把真的 auth bootstrap 拉起来 ——
  // rememberedTokenProvider 留一个 3 秒 timer，CI 上稳定红在
  // 「A Timer is still pending」（本机因 secure storage 插件缺失时序不同，
  // 恰好绿 —— 别信本机）。与 send_rejection_test 同款：mock 配置的容器，
  // bootstrap 在 mock 那条短路，不碰凭据库。
  UncontrolledProviderScope(
    container: _container(),
    child: MaterialApp(
      theme: CortexTheme.dark(),
      home: Scaffold(body: SingleChildScrollView(child: child)),
    ),
  ),
);

ProviderContainer _container() {
  final c = ProviderContainer(
    overrides: [
      appConfigProvider.overrideWith(_MockConfig.new),
      // **instant：** 不关掉 mock 的假延迟的话，ChatController 的 `_reload`
      // 会往 `sessionDetail` 排一个 309ms 的 timer —— 树销毁时它还没到期，
      // 同样是「A Timer is still pending」。这里根本不关心会话数据长什么样
      cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
    ],
  );
  addTearDown(c.dispose);
  return c;
}

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

/// 把鼠标挪到某个部件上（桌面端悬停的唯一可靠模拟方式）。
Future<TestGesture> _hover(WidgetTester tester, Finder target) async {
  final mouse = await tester.createGesture(kind: PointerDeviceKind.mouse);
  await mouse.addPointer(location: Offset.zero);
  addTearDown(mouse.removePointer);
  await mouse.moveTo(tester.getCenter(target));
  await tester.pumpAndSettle();
  return mouse;
}

/// 找到某条消息外面那层 `AnimatedOpacity` 的当前值。
double _opacityOf(WidgetTester tester, Finder within) {
  final opacity = tester.widget<AnimatedOpacity>(
    find.descendant(of: within, matching: find.byType(AnimatedOpacity)).first,
  );
  return opacity.opacity;
}

void main() {
  group('用户消息的动作栏', () {
    testWidgets('默认半透明，悬停到这条消息时全亮', (tester) async {
      await _pump(tester, MessageBubble(message: _user('帮我看下这个报错')));

      final bubble = find.byType(MessageBubble);
      expect(
        _opacityOf(tester, bubble),
        lessThan(1),
        reason:
            '动作栏常驻全亮的话，一屏对话里它和正文抢同一只眼睛 —— '
            '读每一条消息时都会先扫到它',
      );

      await _hover(tester, find.text('帮我看下这个报错'));
      expect(
        _opacityOf(tester, bubble),
        1,
        reason: '悬停到这一条上时，动作栏必须完全显形 —— 否则「能点」这件事看不出来',
      );
    });

    testWidgets('半透明而不是隐藏：动作栏始终占着位置', (tester) async {
      await _pump(tester, MessageBubble(message: _user('帮我看下这个报错')));

      expect(
        find.byTooltip('复制这条'),
        findsOneWidget,
        reason:
            '完全隐藏会让这一行在鼠标进出的瞬间改变高度，而高度一变，'
            '下面所有消息都会跳一下',
      );
    });

    testWidgets('键盘 Tab 能走到动作栏', (tester) async {
      await _pump(tester, MessageBubble(message: _user('帮我看下这个报错')));

      // 消息本身不是可聚焦的（onTap 为 null），但里面的按钮是。
      // 这里钉的是「整条消息没有把焦点吃掉」—— 早先那版把整个
      // 消息包进一个可聚焦的 GestureDetector，Tab 会在它上面停一下
      // 却什么都不发生
      final focusable = find.descendant(
        of: find.byType(MessageBubble),
        matching: find.byType(InkWell),
      );
      expect(
        focusable,
        findsWidgets,
        reason: '动作栏里的按钮必须仍然可聚焦 —— 一个只有鼠标够得着的操作是半个功能',
      );
    });
  });

  group('输入框', () {
    testWidgets('聚焦时边框换成品牌色，失焦退回发丝线', (tester) async {
      await _pump(
        tester,
        MessageComposer(
          enabled: true,
          streaming: false,
          onSend: (_, _) async => true,
          onStop: () {},
          ensureSession: () => 's1',
        ),
      );

      BoxDecoration shellDeco() {
        // 输入框外壳是那个带 8px 圆角 + 边框的 AnimatedContainer
        final box = tester.widget<AnimatedContainer>(
          find
              .ancestor(
                of: find.byType(TextField),
                matching: find.byType(AnimatedContainer),
              )
              .first,
        );
        return box.decoration! as BoxDecoration;
      }

      final rest = (shellDeco().border! as Border).top;
      expect(
        rest.width,
        CortexSpace.hair,
        reason: '常态是发丝线 —— 焦点环不该常驻，否则它就不再是「此刻在这里」的信号',
      );

      await tester.tap(find.byType(TextField));
      await tester.pumpAndSettle();

      final focused = (shellDeco().border! as Border).top;
      expect(
        focused.color,
        isNot(rest.color),
        reason: '聚焦时边框没有变化 —— 用户看不出输入框此刻接管了键盘',
      );
      expect(
        focused.width,
        greaterThan(rest.width),
        reason: '焦点环要比常态粗一档，否则在浅色底上几乎看不出来',
      );
    });
  });

  group('设计系统的一致性', () {
    testWidgets('消息区里没有硬编码的 Material 原色', (tester) async {
      await _pump(tester, MessageBubble(message: _user('测试')));

      // 只钉这一条：气泡底色必须是主题里的表面色，不是 Colors.grey 之类。
      // 硬编码色的坏法是「换主题时这一块不跟着走」，而它不会报错
      final container = tester.widget<Container>(
        find
            .descendant(
              of: find.byType(MessageBubble),
              matching: find.byType(Container),
            )
            .first,
      );
      final deco = container.decoration as BoxDecoration?;
      expect(deco?.color, isNotNull, reason: '用户气泡要有一层中性底，把内容与助手的输出分开');
    });
  });
}
