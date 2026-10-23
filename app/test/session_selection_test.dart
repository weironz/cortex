/// 点一条会话之后，那一行的选中底色**要留住**。
///
/// # 为什么单独一条
///
/// 2026-10-23 用户实报：「切换会话时，定位会话的灰色背景只会一闪而过，
/// 并不会停留在光标所在会话上」。
///
/// 一闪而过 = 底色出现过又消失。它有两个可能的来源，而症状完全一样：
///
/// 1. **悬停态**（鼠标恰好停在这一行上）—— 点完之后列表重建，
///    鼠标底下换了一个元素，`MouseRegion` 的 `onExit` 没送到，
///    或者送到了而新元素没收到 `onEnter`；
/// 2. **选中态**压根没留住 —— `activeSessionId` 被某次刷新改回去了。
///
/// 两者在屏幕上分不开，所以这条测试把鼠标**移开**再断言：
/// 移开之后还亮着，才说明亮的是选中态而不是悬停态。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/theme.dart';
import 'package:cortex_app/features/sessions/session_list.dart';
import 'package:cortex_app/models/chat_session.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

class _FixedApi extends MockCortexApi {
  _FixedApi(this._rows) : super(instant: true);

  final List<ChatSession> _rows;

  @override
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) async => _rows;
}

ProviderContainer _boot(List<ChatSession> rows) => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_MockConfig.new),
    cortexApiProvider.overrideWithValue(_FixedApi(rows)),
    settingsReaderProvider.overrideWithValue(
      () async => const <String, String>{},
    ),
    settingsWriterProvider.overrideWithValue((_) async {}),
    settingsPatcherProvider.overrideWithValue((_, _) async {}),
  ],
);

ChatSession _s(String id) => ChatSession(id: id, title: id);

Future<void> _pump(WidgetTester tester, ProviderContainer c) async {
  tester.view.physicalSize = const Size(320, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: c,
      child: MaterialApp(
        theme: CortexTheme.light(),
        home: const Scaffold(body: SessionList()),
      ),
    ),
  );
  for (var i = 0; i < 10; i++) {
    await tester.pump(const Duration(milliseconds: 60));
  }
}

/// 某一行当前的底色。行是用 `AnimatedContainer` 画的，取它最近的那个。
Color? _rowColor(WidgetTester tester, String title) {
  final box = tester.widget<AnimatedContainer>(
    find
        .ancestor(
          of: find.text(title),
          matching: find.byType(AnimatedContainer),
        )
        .first,
  );
  return (box.decoration as BoxDecoration?)?.color;
}

void main() {
  testWidgets('点一条会话之后，选中底色留得住（鼠标移开也不消失）', (tester) async {
    final c = _boot([_s('alpha'), _s('beta'), _s('gamma')]);
    addTearDown(c.dispose);
    await _pump(tester, c);

    // 先把鼠标挪到别处，确保下面读到的是选中态而不是悬停态
    final mouse = await tester.createGesture(kind: PointerDeviceKind.mouse);
    await mouse.addPointer(location: const Offset(300, 800));
    addTearDown(mouse.removePointer);

    await tester.tap(find.text('beta'));
    await tester.pump();
    for (var i = 0; i < 8; i++) {
      await tester.pump(const Duration(milliseconds: 60));
    }

    expect(
      c.read(chatControllerProvider).activeSessionId,
      'beta',
      reason: '点了 beta 之后控制器里选中的就是 beta —— 这条先钉住状态层',
    );

    final selected = _rowColor(tester, 'beta');
    final other = _rowColor(tester, 'alpha');
    expect(
      selected,
      isNot(other),
      reason:
          '⚠️ 选中那一行的底色与别的行一样 —— 用户看不出自己在哪条会话上。'
          '「一闪而过」正是这个形状：点下去那一帧亮了一下，之后被重建抹平',
    );
    expect(selected, isNotNull, reason: '选中底色不该是 null（透明）');
  });

  testWidgets('选中态在列表刷新之后仍然留着', (tester) async {
    final c = _boot([_s('alpha'), _s('beta')]);
    addTearDown(c.dispose);
    await _pump(tester, c);

    c.read(chatControllerProvider.notifier).selectSession('beta');
    await tester.pump();
    for (var i = 0; i < 6; i++) {
      await tester.pump(const Duration(milliseconds: 60));
    }
    final before = _rowColor(tester, 'beta');

    // 一次刷新（WebSocket bump / 切归档 / 别的设备改了什么都走这条路）
    await c.read(chatControllerProvider.notifier).loadSessions();
    await tester.pump();
    for (var i = 0; i < 6; i++) {
      await tester.pump(const Duration(milliseconds: 60));
    }

    expect(
      c.read(chatControllerProvider).activeSessionId,
      'beta',
      reason: '刷新不该把用户从正在看的会话上踢走',
    );
    expect(
      _rowColor(tester, 'beta'),
      before,
      reason: '刷新之后选中底色变了 —— 那正是「一闪而过」的第二个来源',
    );
  });

  testWidgets('鼠标从选中行移到别的行，选中底色不跟着走', (tester) async {
    // 这一条复现的正是用户报的那个动作：**鼠标停在 A 上点它，然后移开**。
    // 移开之后 A 必须仍然是选中色，而 B 只在悬停时亮。
    final c = _boot([_s('alpha'), _s('beta'), _s('gamma')]);
    addTearDown(c.dispose);
    await _pump(tester, c);

    final mouse = await tester.createGesture(kind: PointerDeviceKind.mouse);
    await mouse.addPointer(location: Offset.zero);
    addTearDown(mouse.removePointer);

    // 悬停到 beta，然后点它
    await mouse.moveTo(tester.getCenter(find.text('beta')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('beta'));
    await tester.pump();
    for (var i = 0; i < 8; i++) {
      await tester.pump(const Duration(milliseconds: 60));
    }

    final selectedColor = _rowColor(tester, 'beta');
    expect(
      c.read(chatControllerProvider).activeSessionId,
      'beta',
      reason: '点下去之后选中的应当是 beta',
    );

    // 鼠标移到 gamma
    await mouse.moveTo(tester.getCenter(find.text('gamma')));
    await tester.pumpAndSettle();

    expect(
      _rowColor(tester, 'beta'),
      selectedColor,
      reason:
          '⚠️ 鼠标移开之后 beta 的底色变了 —— 这正是「一闪而过」：'
          '亮的是悬停态，选中态根本没留住',
    );
    expect(
      _rowColor(tester, 'gamma'),
      isNot(selectedColor),
      reason: '悬停态不该与选中态同色，否则鼠标扫过时每一行都像被选中了',
    );
  });
}
