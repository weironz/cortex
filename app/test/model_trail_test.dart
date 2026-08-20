/// 每条回复旁边那个「是谁写的」。
///
/// # 这一组盯的四件事
///
/// 1. **不知道就什么都不画。** 迁移之前的历史、导入进来的记录、以及老
///    服务端，这一列都是空。画一个「未知模型」或者猜一个「默认模型」
///    填上去，等于对历史撒谎，而用户没有任何办法看出那是猜的。
/// 2. **查不到显示名就用原始 id。** 目录跟着 models.dev 走，新发的型号
///    有一段空窗期 —— 那时原始 id 仍然是有用信息，比「未知」强得多。
/// 3. **目录没拉到也照画。** 等目录才画的话，一个连不上 `/llm/models`
///    的部署里这一行永远不出现，而那与「这条消息没记模型」看起来一样。
/// 4. **流式过程中不画。** 那时还不知道是谁答的，先画一个再换掉会闪。
library;

import 'package:cortex_app/features/chat/widgets/message_bubble.dart';
import 'package:cortex_app/models/chat_message.dart';
import 'package:cortex_app/models/model_option.dart';
import 'package:cortex_app/state/model_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

const _catalog = ModelCatalog(
  provider: 'google',
  defaultModel: 'gemini-3-pro',
  models: [
    ModelOption(
      id: 'gemini-3-pro',
      displayName: 'Gemini 3 Pro',
      source: 'src-a',
      toolCall: true,
    ),
    ModelOption(
      id: 'qwen-turbo',
      displayName: 'Qwen Turbo',
      source: 'src-a',
      toolCall: true,
    ),
  ],
);

/// 画一条 assistant 消息。[catalog] 传 null 模拟「目录没拉到」。
Future<void> _pump(
  WidgetTester tester, {
  required List<String> models,
  ModelCatalog? catalog = _catalog,
  bool streaming = false,
}) async {
  tester.view.physicalSize = const Size(1400, 900);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    ProviderScope(
      overrides: [
        modelCatalogProvider.overrideWith((ref) async {
          if (catalog == null) throw Exception('目录拉不到');
          return catalog;
        }),
      ],
      child: MaterialApp(
        home: Scaffold(
          body: streaming
              ? AssistantBlock(
                  text: '正在写…',
                  toolCalls: const [],
                  streaming: true,
                  models: models,
                )
              : MessageBubble(
                  message: ChatMessage(
                    id: 'm1',
                    role: MessageRole.assistant,
                    text: '答完了',
                    createdAt: DateTime(2026, 8, 20, 10),
                    models: models,
                  ),
                ),
        ),
      ),
    ),
  );
  // ⚠️ 流式那一支**不能 pumpAndSettle** —— 它画的是一个不会停的光标/
  // 思考指示器动画（那正是它该有的样子），settle 会一直等到超时
  if (streaming) {
    await tester.pump(const Duration(milliseconds: 50));
  } else {
    await tester.pumpAndSettle();
  }
}

void main() {
  testWidgets('认得出的型号显示目录里的名字', (tester) async {
    await _pump(tester, models: ['gemini-3-pro']);
    expect(find.text('Gemini 3 Pro'), findsOneWidget);
  });

  testWidgets('一轮用过好几个就按顺序连起来', (tester) async {
    await _pump(tester, models: ['qwen-turbo', 'gemini-3-pro']);
    expect(
      find.text('Qwen Turbo → Gemini 3 Pro'),
      findsOneWidget,
      reason:
          '「自动」档会先用便宜的跑工具、再用贵的写答案。只显示一个'
          '就把这件事整个抹掉了，而顺序本身也是信息',
    );
  });

  testWidgets('目录里查不到的显示原始 id，不显示「未知」', (tester) async {
    await _pump(tester, models: ['gemini-4-ultra-brandnew']);
    expect(
      find.text('gemini-4-ultra-brandnew'),
      findsOneWidget,
      reason:
          '目录跟着 models.dev 走，新发的型号有一段空窗期 —— '
          '那时原始 id 仍然是有用信息，比一句「未知模型」强得多',
    );
  });

  testWidgets('目录没拉到也照画（用原始 id）', (tester) async {
    await _pump(tester, models: ['gemini-3-pro'], catalog: null);
    expect(
      find.text('gemini-3-pro'),
      findsOneWidget,
      reason:
          '等目录才画的话，一个连不上 /llm/models 的部署里这一行'
          '永远不出现 —— 而那与「这条消息没记模型」看起来一模一样',
    );
  });

  testWidgets('不知道就什么都不画', (tester) async {
    await _pump(tester, models: const []);

    // 头部那一行只该有「Cortex」与时间，不该多出任何模型标签
    expect(find.text('Cortex'), findsOneWidget);
    expect(
      find.textContaining('未知'),
      findsNothing,
      reason:
          '迁移之前的历史、导入进来的记录都是空。画一个「未知模型」'
          '或者猜一个默认模型填上去，等于对历史撒谎',
    );
    expect(find.text('Gemini 3 Pro'), findsNothing);
    // ⚠️ **结构性断言，不能只查文字。**
    //
    // 第一版这条只查了「未知」与具体名字，两者在空列表下本来就找不到 ——
    // 于是把「空的也照画」这个 bug 注入进去之后，测试仍然是绿的：
    // 它画的是一个 `Text('')`，肉眼看不见，断言也碰不到。
    // 直接查「有没有画出一个空标签」才有区分力。
    expect(
      find.byWidgetPredicate(
        // 只查「data 是空串」的。`data == null` 是 `Text.rich`
        // （markdown 渲染器就用它），与这里无关
        (w) => w is Text && w.data != null && w.data!.isEmpty,
      ),
      findsNothing,
      reason:
          '不知道就一个字都不该画 —— 画一个空 Text 虽然看不见，'
          '但它会占位、会让头部那一行多一段间距',
    );
  });

  testWidgets('流式过程中不画', (tester) async {
    await _pump(tester, models: ['gemini-3-pro'], streaming: true);
    expect(
      find.text('Gemini 3 Pro'),
      findsNothing,
      reason:
          '那时还不知道是谁答的（模型名在流末尾的用量里才回来）—— '
          '先画一个再换掉会闪',
    );
  });
}
