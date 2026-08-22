/// 「正在创建图片」那块占位，以及它靠什么出现 / 消失。
///
/// # 这一组盯的两件事
///
/// 1. **占位跟着那次调用的生死走**，不是跟着「这一轮在不在跑」。一轮里
///    可能先读文件再画图，前半段画一块空图位是在承诺一件还没发生的事。
/// 2. **两次同名调用要画成两行。** `ToolCall.merge` 从前靠「上一行同名且
///    还 pending」猜这是不是结果 —— 并行调用下第二次会被当成第一次的结果
///    吃掉，于是两张图只出现一张，而且不报错。
library;

import 'package:cortex_app/features/chat/widgets/message_bubble.dart';
import 'package:cortex_app/features/images/widgets/drawing_placeholder.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/tool_call.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

Future<void> _pump(WidgetTester tester, List<ToolCall> calls) async {
  tester.view.physicalSize = const Size(900, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    ProviderScope(
      child: MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: AssistantBlock(text: '', toolCalls: calls, streaming: true),
          ),
        ),
      ),
    ),
  );
  await tester.pump(const Duration(milliseconds: 120));
}

void main() {
  group('生成中的占位', () {
    testWidgets('generate_image 还没回结果时占住那张图将来的位置', (tester) async {
      await _pump(tester, const [ToolCall(name: 'generate_image')]);
      expect(find.byType(DrawingPlaceholder), findsOneWidget);
      expect(
        find.text('正在创建图片'),
        findsOneWidget,
        reason:
            '一个转圈图标在第二十秒读起来就是「卡住了」 —— '
            '生图要几十秒，界面要说清它在干活',
      );
      expect(tester.takeException(), isNull);
    });

    testWidgets('结果回来了就撤掉', (tester) async {
      await _pump(tester, const [
        ToolCall(name: 'generate_image', result: '生成好了 1 张图'),
      ]);
      expect(
        find.byType(DrawingPlaceholder),
        findsNothing,
        reason: '图这时已经在附件里了 —— 占位再留着就是同一张图占两个位置',
      );
    });

    testWidgets('别的工具在跑时不画图位', (tester) async {
      await _pump(tester, const [ToolCall(name: 'read_file')]);
      expect(
        find.byType(DrawingPlaceholder),
        findsNothing,
        reason:
            '判据是「有一次 generate_image 还没回结果」，'
            '不是「这一轮在跑」—— 一轮里可能先读文件再画图，'
            '前半段画一块空图位是在承诺一件还没发生的事',
      );
    });

    testWidgets('画几张就占几个位', (tester) async {
      await _pump(tester, const [
        ToolCall(name: 'generate_image'),
        ToolCall(name: 'generate_image'),
      ]);
      expect(find.byType(DrawingPlaceholder), findsNWidgets(2));
    });
  });

  group('ToolCall.merge 的配对', () {
    test('result 合进上一条同名的未完成行', () {
      var calls = ToolCall.merge(
        const [],
        'generate_image',
        '调用 generate_image',
        phase: ToolPhase.call,
      );
      expect(calls, hasLength(1));
      expect(calls.single.pending, isTrue);

      calls = ToolCall.merge(
        calls,
        'generate_image',
        'generate_image 生成好了',
        phase: ToolPhase.result,
      );
      expect(calls, hasLength(1), reason: '一次调用只该画一行');
      expect(calls.single.pending, isFalse);
    });

    test('两次并行的同名调用画成两行', () {
      var calls = ToolCall.merge(
        const [],
        'generate_image',
        '调用 generate_image',
        phase: ToolPhase.call,
      );
      calls = ToolCall.merge(
        calls,
        'generate_image',
        '调用 generate_image',
        phase: ToolPhase.call,
      );
      expect(
        calls,
        hasLength(2),
        reason:
            '旧规则靠「上一行同名且还 pending」猜这是不是结果 —— '
            '于是第二次调用被当成第一次的结果吃掉，两张图只出现一张，'
            '而且不报错、不崩，只是少了一样东西',
      );
      expect(calls.every((c) => c.pending), isTrue);
    });

    test('老服务端不发 phase 时退回旧行为', () {
      var calls = ToolCall.merge(const [], 'read_file', '调用 read_file');
      calls = ToolCall.merge(calls, 'read_file', 'read_file 读了 3 行');
      expect(calls, hasLength(1), reason: '缺省是 result —— 老服务端上配对照旧成立');
      expect(calls.single.pending, isFalse);
    });
  });
}
