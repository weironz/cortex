/// 「添加模型」弹窗 —— 能力筛选，以及**「0」要能解释得清楚**。
///
/// # 这一组盯的那件事
///
/// 2026-08-20：用户在这个弹窗里搜 `image`，搜出一屏 `gemini-*-image`，
/// 而筛选栏写着「能生图 0」。他只能来问「为什么这些模型不支持」。
///
/// 那个 0 本身没错（那时 Google 的生图接口确实没接，点了也画不出来），
/// 错的是它**一个字都没解释**：界面上那些型号与普通对话模型长得一样，
/// 用户读出来的是「这些模型不会画画」—— 既是错的，又把责任推给了模型。
///
/// 所以这里钉两件事：会画但我们没接的，逐条要说；筛「能生图」筛出空列表
/// 时，空状态也要说。
library;

import 'package:cortex_app/features/settings/pages/model_add_dialog.dart';
import 'package:cortex_app/models/model_source.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// 一份**混齐四种形态**的型号表。
///
/// 少任何一种，下面的断言就失去区分力 —— 全都是「没接」的话，
/// 「接了的不该挂这句话」那条永远绿。
const _fetched = FetchedModels(
  live: true,
  models: [
    // 接了的：能选，也真画得出来
    FetchedModel(
      id: 'gemini-3-pro-image-preview',
      displayName: 'gemini-3-pro-image-preview',
      toolCall: false,
      imageOutput: true,
    ),
    // ⚠️ 会画，但我们没接这家 —— 这一条就是那次提问的现场
    FetchedModel(
      id: 'gpt-image-1',
      displayName: 'gpt-image-1',
      toolCall: false,
      imageOutput: false,
      imageUnwired: true,
    ),
    // 普通对话模型：不该挂任何生图相关的话
    FetchedModel(
      id: 'gpt-5.2',
      displayName: 'GPT-5.2',
      toolCall: true,
      imageOutput: false,
      context: 400000,
      inputMicrosPerMtok: 1250000,
      outputMicrosPerMtok: 10000000,
    ),
    // 目录里查不到的：三个字段全 null =「不知道」
    FetchedModel(
      id: 'some-brand-new-model',
      displayName: 'some-brand-new-model',
    ),
  ],
);

Future<void> _open(WidgetTester tester, {FetchedModels? fetched}) async {
  tester.view.physicalSize = const Size(1200, 1600);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: Builder(
          builder: (ctx) => TextButton(
            onPressed: () => showAddModels(
              ctx,
              fetched: fetched ?? _fetched,
              already: const [],
            ),
            child: const Text('开'),
          ),
        ),
      ),
    ),
  );
  await tester.tap(find.text('开'));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('会画但我们没接的，逐条说清楚是我们的缺口', (tester) async {
    await _open(tester);

    expect(
      find.textContaining('还没接这家的生图接口'),
      findsOneWidget,
      reason:
          '差这一句的表现就是用户盯着一屏生图模型问「为什么都不支持」—— '
          '而界面上它与普通对话模型长得一模一样',
    );
  });

  testWidgets('接了的那家不挂这句话', (tester) async {
    await _open(tester);

    final row = find.ancestor(
      of: find.text('gemini-3-pro-image-preview'),
      matching: find.byType(CheckboxListTile),
    );
    expect(
      find.descendant(of: row, matching: find.textContaining('还没接')),
      findsNothing,
      reason:
          'Google 的生图已经接上了。挂着这句话等于告诉用户一个能用的'
          '功能不能用 —— 与漏说是同一类错，方向相反',
    );
  });

  testWidgets('普通对话模型不挂生图相关的话', (tester) async {
    await _open(tester);

    final row = find.ancestor(
      of: find.text('GPT-5.2'),
      matching: find.byType(CheckboxListTile),
    );
    expect(
      find.descendant(of: row, matching: find.textContaining('生图接口')),
      findsNothing,
      reason:
          '它本来就不是生图模型。按名字瞎猜的话，一个正常型号上会冒出'
          '一句莫名其妙的话',
    );
  });

  testWidgets('筛「能生图」筛出空列表时，空状态要解释那个 0', (tester) async {
    await _open(
      tester,
      // 这一家只有「会画但没接」的，筛出来必然是空
      fetched: const FetchedModels(
        live: true,
        models: [
          FetchedModel(
            id: 'gpt-image-1',
            displayName: 'gpt-image-1',
            imageOutput: false,
            imageUnwired: true,
          ),
          FetchedModel(
            id: 'gpt-image-1.5',
            displayName: 'gpt-image-1.5',
            imageOutput: false,
            imageUnwired: true,
          ),
        ],
      ),
    );

    // 点**筛选 chip**，不是 `textContaining('能生图')` —— 那会同时匹配到
    // 每一行那句「它能生图，但我们还没接…」，找到 3 个然后报歧义
    await tester.tap(find.widgetWithText(FilterChip, '能生图 0'));
    await tester.pumpAndSettle();

    expect(
      find.textContaining('没有符合条件的型号'),
      findsNothing,
      reason: '光说「没有符合条件的」等于让用户以为这家不会画画',
    );
    expect(
      find.textContaining('2 个会生图的型号'),
      findsOneWidget,
      reason: '要说出数量与原因 —— 0 的原因是我们的缺口，不是这家没有',
    );
  });

  testWidgets('这家真的一个生图模型都没有时，就说没有', (tester) async {
    await _open(
      tester,
      fetched: const FetchedModels(
        live: true,
        models: [
          FetchedModel(
            id: 'deepseek-v4-pro',
            displayName: 'DeepSeek V4 Pro',
            toolCall: true,
            imageOutput: false,
          ),
        ],
      ),
    );

    // 点**筛选 chip**，不是 `textContaining('能生图')` —— 那会同时匹配到
    // 每一行那句「它能生图，但我们还没接…」，找到 3 个然后报歧义
    await tester.tap(find.widgetWithText(FilterChip, '能生图 0'));
    await tester.pumpAndSettle();

    expect(
      find.textContaining('没有符合条件的型号'),
      findsOneWidget,
      reason:
          '没有就是没有。这时扯一句「我们没接」是编的 —— '
          'DeepSeek 根本没有生图模型',
    );
  });
}
