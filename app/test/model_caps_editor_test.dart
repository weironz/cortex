/// 「编辑模型」面板 —— 手工按下一个模型能干什么。
///
/// # 这一组只盯一件事：**三态不能塌成二值**
///
/// 这个仓库为了「不知道 ≠ 不支持」改过三处判据：发送时「不知道」放行
/// （与服务端 `ensure_can_see` 同一条）、筛选时「不知道」不算有、徽标在
/// 「不知道」时不画。
///
/// 而 Cherry Studio 的同名面板用的是**二值 chip**（点亮=支持，不亮=不支持）。
/// 照抄它、或者哪天有人觉得三个 chip 太啰嗦改成开关，后果都是同一个：
/// 用户只要打开过这个面板并保存，那些他压根没意见的位就全被按成
/// 「不支持」了 —— 而他什么都没点。那会把一批能用的模型静默挡在门外，
/// 正是这个面板要解决的问题的反面。
///
/// 所以这里断言的不是「界面长什么样」，是**发出去的那份记录里，
/// 没碰过的位必须是 null**。
library;

import 'package:cortex_app/features/settings/widgets/model_caps_editor.dart';
import 'package:cortex_app/models/model_source.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// 一个目录说得出 vision、说不出工具的模型 —— 两种自动结论都覆盖到。
const _model = FetchedModel(
  id: 'deepseek-v4-flash-vision-exp',
  displayName: 'DeepSeek V4 Flash Vision Exp',
  context: 1048576,
  vision: true,
  toolCall: null,
  reasoning: false,
  // 明确给上，好让「说不出」只出现在工具那一组 —— 不给的话它也是 null，
  // 断言数量时会数出两个，而那与要测的东西无关
  imageOutput: false,
);

Future<CapsOverride?> _open(
  WidgetTester tester, {
  FetchedModel model = _model,
}) async {
  CapsOverride? saved;
  tester.view.physicalSize = const Size(1000, 1600);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: ModelCapsEditor(
          model: model,
          sourceLabel: '我的中转站',
          canProbe: false,
          busy: false,
          onSave: (c) => saved = c,
        ),
      ),
    ),
  );
  await tester.pump(const Duration(milliseconds: 50));
  return saved;
}

/// 保存按钮此刻能不能按。
bool _canSave(WidgetTester tester) =>
    tester.widget<FilledButton>(find.byType(FilledButton)).onPressed != null;

void main() {
  testWidgets('查得到的预先选中（浅色），查不到的两个都不选中', (tester) async {
    await _open(tester);

    expect(
      find.text('自动判断的，你还没改过'),
      findsWidgets,
      reason: '选中态得说清是谁下的结论 —— 否则用户不知道该不该动它',
    );
    expect(
      find.text('没查到 —— 你选一个会更准，不选也照样能用'),
      findsOneWidget,
      reason:
          '工具那一位查不到（null），两个 chip 都不选中。'
          '**必须说「不选也能用」**：一个空着的选择看起来像「你必须先选一个」，'
          '而事实相反 —— 不确定是放行的',
    );
    expect(find.text('你改的'), findsNothing, reason: '一位都没按过，不该有任何「你改的」标记');
    expect(_canSave(tester), isFalse, reason: '什么都没改，保存该是灰的');
  });

  testWidgets('⚠️ 只按一位，其余几位必须发 null —— 而不是 false', (tester) async {
    CapsOverride? saved;
    tester.view.physicalSize = const Size(1000, 1600);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ModelCapsEditor(
            model: _model,
            sourceLabel: '我的中转站',
            canProbe: false,
            busy: false,
            onSave: (c) => saved = c,
          ),
        ),
      ),
    );
    await tester.pump(const Duration(milliseconds: 50));

    // 只动「工具」那一位。面板顺序是 视觉/工具/思考/生图，每位两个 chip，
    // 所以「支持」的第 2 个（at(1)）是工具那一组 —— **不能用 .first**，
    // 那是视觉，而这条测试的全部意义就是「没碰过的视觉必须保持 null」
    await tester.tap(find.text('支持').at(1));
    await tester.pump(const Duration(milliseconds: 50));
    expect(_canSave(tester), isTrue, reason: '改过之后保存该亮起来');

    await tester.tap(find.byType(FilledButton));
    await tester.pump(const Duration(milliseconds: 50));

    final c = saved;
    expect(c, isNotNull, reason: '点了保存却没把记录交出来');
    expect(c!.toolCall, isTrue, reason: '按下的那一位要如实发出去');
    expect(
      c.vision,
      isNull,
      reason:
          '⚠️ 视觉那一位用户一下都没碰过，就必须是 null（没意见）。'
          '发成 false 的话，他只是想标一下工具，却把一个真能看图的模型'
          '按成了看不懂图 —— 而他什么都没点。这正是三态塌成二值的代价',
    );
    expect(
      c.reasoning,
      isNull,
      reason:
          '思考那一位自动说的是 false，但用户没按过 —— '
          '把自动结论固化进覆盖记录同样是错的：目录哪天更新了，'
          '这个模型会一直停在旧答案上',
    );
    expect(c.displayName, isNull, reason: '名字没填，不该发一个空串');
    expect(c.context, isNull, reason: '上下文没填，不该发 0');
  });

  testWidgets('按过的那一位标出「你改的」', (tester) async {
    await _open(
      tester,
      model: const FetchedModel(
        id: 'x',
        vision: false,
        overridden: CapsOverride(vision: true),
      ),
    );
    expect(
      find.text('你改的'),
      findsOneWidget,
      reason: '不标的话，下次打开只看到一个结论，分不清是目录说的还是自己按的，于是不敢动',
    );
  });

  testWidgets('改过的那一位能单独「改回自动」', (tester) async {
    CapsOverride? saved;
    tester.view.physicalSize = const Size(1000, 1600);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ModelCapsEditor(
            model: const FetchedModel(
              id: 'x',
              vision: false,
              overridden: CapsOverride(vision: true),
            ),
            sourceLabel: '我的中转站',
            canProbe: false,
            busy: false,
            onSave: (c) => saved = c,
          ),
        ),
      ),
    );
    await tester.pump(const Duration(milliseconds: 50));

    await tester.tap(find.text('改回自动'));
    await tester.pump(const Duration(milliseconds: 50));
    expect(find.text('你改的'), findsNothing, reason: '点了「改回自动」，那一位就不再是他的断言了');

    await tester.tap(find.byType(FilledButton));
    await tester.pump(const Duration(milliseconds: 50));
    expect(
      saved?.vision,
      isNull,
      reason:
          '改回自动 = 这一位交还给自动判断。存成 false 的话，'
          '它会以「用户说不支持」的身份把自动结论永久压住',
    );
  });

  testWidgets('「全部改回自动」把整条覆盖清掉', (tester) async {
    CapsOverride? saved;
    tester.view.physicalSize = const Size(1000, 1600);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ModelCapsEditor(
            model: const FetchedModel(
              id: 'x',
              vision: false,
              overridden: CapsOverride(vision: true, displayName: '我改的名字'),
            ),
            sourceLabel: '我的中转站',
            canProbe: false,
            busy: false,
            onSave: (c) => saved = c,
          ),
        ),
      ),
    );
    await tester.pump(const Duration(milliseconds: 50));

    await tester.tap(find.text('全部改回自动'));
    await tester.pump(const Duration(milliseconds: 50));
    await tester.tap(find.byType(FilledButton));
    await tester.pump(const Duration(milliseconds: 50));

    expect(
      saved?.isEmpty,
      isTrue,
      reason:
          '「全部改回自动」之后发出去的必须是一条空记录 —— '
          '服务端据此把整条删掉，而不是存一堆 false',
    );
  });
}
