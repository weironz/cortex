/// 输入框底部的三档权限开关。
///
/// # 这几条各自盯着什么
///
/// 权限开关最容易出的错不是「点了没反应」（那当场就发现了），而是
/// **默认值倒向了放行**、**档位没跟着请求走**、**危险那一档没有门槛**。
/// 三样都不会报错，只会让一台机器悄悄变成无人值守的。
library;

import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/features/chat/widgets/permission_mode_chip.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

Widget _wrap(Widget child) => ProviderScope(
  child: MaterialApp(home: Scaffold(body: Center(child: child))),
);

void main() {
  group('档位本身', () {
    test('默认是最谨慎的一档', () {
      expect(
        PermissionMode.values.first,
        PermissionMode.ask,
        reason: '枚举的第一个值就是 Dart 侧的默认。倒向放行的话，'
            '任何忘了传档位的调用点都会静默拿到无人值守的执行权',
      );
    });

    test('线上取值与服务端的 snake_case 逐字对齐', () {
      expect(PermissionMode.ask.wire, 'ask');
      expect(
        PermissionMode.acceptEdits.wire,
        'accept_edits',
        reason: 'Rust 那侧是 serde(rename_all = "snake_case")。写成 acceptEdits '
            '的话服务端认不出来，而它的处理是**回落到默认档** —— '
            '于是用户选了「自动改文件」却还在被逐条询问，且没有任何报错',
      );
      expect(PermissionMode.bypass.wire, 'bypass');
    });

    test('认不出的取值回落到「问」，不是回落到放行', () {
      for (final bad in [null, '', 'BYPASS', 'yolo', 'accept-edits']) {
        expect(
          PermissionMode.fromWire(bad),
          PermissionMode.ask,
          reason: '$bad 应当落回最谨慎的一档。老版本存下的值、手改坏的配置、'
              '将来删掉的档位都会走到这里，而回落方向错了就是一次静默提权',
        );
      }
    });

    test('每一档都说清了「哪些还会问」', () {
      expect(
        PermissionMode.acceptEdits.blurb.contains('执行命令'),
        isTrue,
        reason: '「自动改文件」不等于「改哪里都不问」—— 它只关掉写文件那一半。'
            '描述里不点明的话，用户以为自己只省了几次点击，'
            '实际上会以为工作区外也放开了',
      );
      expect(
        PermissionMode.acceptEdits.blurb.contains('工作区外'),
        isTrue,
        reason: '越界与风险是两件独立的事，这一档不关越界确认',
      );
    });
  });

  group('chip', () {
    testWidgets('显示当前档位', (tester) async {
      await tester.pumpWidget(_wrap(const PermissionModeChip()));
      await tester.pump();
      expect(find.text('逐条确认'), findsOneWidget);
    });

    testWidgets('点开能选到「自动改文件」，且真的写进了 provider', (tester) async {
      late ProviderContainer container;
      await tester.pumpWidget(
        _wrap(
          Consumer(
            builder: (ctx, ref, _) {
              container = ProviderScope.containerOf(ctx);
              return const PermissionModeChip();
            },
          ),
        ),
      );
      await tester.pump();

      await tester.tap(find.text('逐条确认'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('自动改文件'));
      await tester.pumpAndSettle();

      expect(
        container.read(permissionModeProvider),
        PermissionMode.acceptEdits,
        reason: '选完档位没写进 provider 的话，chip 上显示变了但**请求还是老档位**'
            '—— 而那个差异只有真去数弹窗次数才看得出来',
      );
    });

    testWidgets('选「完全放行」要先过一道确认；取消则一切照旧', (tester) async {
      late ProviderContainer container;
      await tester.pumpWidget(
        _wrap(
          Consumer(
            builder: (ctx, ref, _) {
              container = ProviderScope.containerOf(ctx);
              return const PermissionModeChip();
            },
          ),
        ),
      );
      await tester.pump();

      await tester.tap(find.text('逐条确认'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('完全放行'));
      await tester.pumpAndSettle();

      expect(
        find.text('关掉全部确认？'),
        findsOneWidget,
        reason: '另外两档改的是打扰频率，选错了下一次弹窗就知道了；'
            '这一档改的是**有没有人在把关**，而这件事在界面上没有别的症状',
      );
      await tester.tap(find.text('算了'));
      await tester.pumpAndSettle();

      expect(
        container.read(permissionModeProvider),
        PermissionMode.ask,
        reason: '用户点了「算了」，档位却已经切过去 —— 那正是这道确认要防的事',
      );
    });

    testWidgets('确认之后 chip 变成警示样式', (tester) async {
      await tester.pumpWidget(_wrap(const PermissionModeChip()));
      await tester.pump();

      await tester.tap(find.text('逐条确认'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('完全放行'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('我明白，关掉'));
      await tester.pumpAndSettle();

      expect(find.text('完全放行'), findsOneWidget);
      expect(
        find.byIcon(Icons.lock_open_rounded),
        findsOneWidget,
        reason: '开着的时候必须一直看得见。做成一个普通样式的 chip，'
            '人三天后就忘了自己关过确认了',
      );
    });
  });
}
