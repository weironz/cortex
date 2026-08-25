/// 「默认模型」那一页 —— 把角色指派给某条来源上的某个型号。
///
/// # 这一组盯的四件事
///
/// 1. **绘画那一行只列画得出来的。** 不筛的话，用户会在一屏对话模型里
///    挑一个，然后在保存那一刻吃服务端一个 400 ——「`x` 生不了图」，
///    而他看不出哪个能选。
/// 2. **绘画那一行不能照搬「不支持工具调用就拦」。** 生图模型基本都不
///    支持工具调用，照搬的话这一行会把它该提供的每一项都画成灰的。
/// 3. **保存是整份发的。** 只发改的那一条会把另外两个角色清掉 ——
///    而用户改的是「快速模型」，丢的是「主模型」，两者毫无关联，
///    他根本不会往这儿想。
/// 4. **没指派时说的是实际会用哪个**，不是「未设置」。三个角色不指派时
///    各走各的回落，「未设置」三个字读不出这一点。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/settings/pages/model_roles_page.dart';
import 'package:cortex_app/models/model_option.dart';
import 'package:cortex_app/models/model_role.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

/// 一份**混齐四种形态**的目录：能跑 agent 的、能生图的、
/// 两样都不行的、以及目录里查不到的（能力字段全 null）。
///
/// 少任何一种，下面那几条筛选断言就失去区分力 —— 全都能跑 agent 的话，
/// 「拦住不能跑 agent 的」这条测试永远绿。
const _catalog = ModelCatalog(
  provider: 'deepseek',
  defaultModel: 'deepseek-v4-pro',
  cheapModel: 'deepseek-v4-flash',
  autoAvailable: true,
  models: [
    ModelOption(
      id: 'deepseek-v4-pro',
      displayName: 'DeepSeek V4 Pro',
      source: 'deployment',
      sourceLabel: '部署提供',
      toolCall: true,
      imageOutput: false,
      inputMicrosPerMtok: 430000,
    ),
    ModelOption(
      id: 'qwen3-max',
      displayName: 'Qwen3 Max',
      source: 'src-a',
      sourceLabel: '通义千问',
      freeOfQuota: true,
      toolCall: true,
      imageOutput: false,
      inputMicrosPerMtok: 600000,
    ),
    // 生图模型：**不支持工具调用**，这正是绘画那一行不能照搬另外两行
    // 规则的原因
    ModelOption(
      id: 'qwen-image-plus',
      displayName: 'Qwen Image Plus',
      source: 'src-a',
      sourceLabel: '通义千问',
      freeOfQuota: true,
      toolCall: false,
      imageOutput: true,
    ),
    // ⚠️ 生图模型的**真实形态**：目录里查不到，所以 `tool_call` 是
    // null（不知道），只有 `image_output` 说得出它是画画的。
    //
    // 2026-08-20 在真实账号上就是这样 —— 20 个 qwen-image 全都
    // `tool_call == null`，于是「不知道就放行」把它们原样摆进了主模型
    // 选择器。上面那个 `toolCall: false` 的假数据**测不出**这件事。
    ModelOption(
      id: 'qwen-image-2.0',
      displayName: 'qwen-image-2.0',
      source: 'src-a',
      sourceLabel: '通义千问',
      freeOfQuota: true,
      imageOutput: true,
    ),
    // 目录里查不到的：三个能力字段全 null =「不知道」，不是「不行」
    ModelOption(
      id: 'MiniMax-M2.5',
      displayName: 'MiniMax-M2.5',
      source: 'src-a',
      sourceLabel: '通义千问',
      freeOfQuota: true,
    ),
  ],
);

/// 记下每一次整份替换发了什么。
class _Api extends MockCortexApi {
  _Api({RoleAssignments? initial})
    : _roles = initial ?? const RoleAssignments(),
      super(instant: true);

  RoleAssignments _roles;
  final List<List<String>> saves = [];

  @override
  Future<ModelCatalog> llmModels() async => _catalog;

  @override
  Future<RoleAssignments> modelRoles() async => _roles;

  @override
  Future<RoleAssignments> saveModelRoles(RoleAssignments roles) async {
    saves.add(
      roles.roles.map((a) => '${a.role.wire}=${a.model}').toList()..sort(),
    );
    _roles = roles;
    return roles;
  }
}

ProviderContainer _boot(_Api api) => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_MockConfig.new),
    cortexApiProvider.overrideWithValue(api),
    settingsReaderProvider.overrideWithValue(
      () async => const <String, String>{},
    ),
    settingsWriterProvider.overrideWithValue((_) async {}),
  ],
);

Future<void> _pump(WidgetTester tester, ProviderContainer c) async {
  tester.view.physicalSize = const Size(1200, 1400);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: c,
      child: const MaterialApp(home: Scaffold(body: ModelRolesPage())),
    ),
  );
  await tester.pumpAndSettle();
}

/// 打开某个角色的选择器。
Future<void> _openPicker(WidgetTester tester, String roleLabel) async {
  await tester.tap(find.text(roleLabel));
  await tester.pumpAndSettle();
}

/// 弹层里某个型号那一行能不能点。
///
/// 2026-08-25 起选择器是紧凑弹层，行是 `InkWell` 而不是 `ListTile` ——
/// 「不给选」的落点是 `onTap == null`。取 `.first`：`find.ancestor`
/// 从最近的祖先往外排，最近那个 InkWell 就是行本身。
bool _rowEnabled(WidgetTester tester, String name) {
  final row = tester.widget<InkWell>(
    find.ancestor(of: find.text(name), matching: find.byType(InkWell)).first,
  );
  return row.onTap != null;
}

void main() {
  testWidgets('三个角色都在，没有第四个', (tester) async {
    final c = _boot(_Api());
    addTearDown(c.dispose);
    await _pump(tester, c);

    for (final r in ModelRole.values) {
      expect(find.text(r.label), findsOneWidget, reason: '${r.label} 那一行不见了');
    }
    expect(
      find.text('翻译模型'),
      findsNothing,
      reason:
          '我们没有翻译功能。摆一个没人调用的角色出来，'
          '用户会以为自己配的东西在起作用',
    );
  });

  testWidgets('没指派时说的是实际会用哪个，不是「未设置」', (tester) async {
    final c = _boot(_Api());
    addTearDown(c.dispose);
    await _pump(tester, c);

    expect(
      find.textContaining('跟随部署 · deepseek-v4-pro'),
      findsOneWidget,
      reason:
          '主模型不指派时走部署那个 —— 写「未设置」的话，'
          '用户读不出现在到底在用什么',
    );
    expect(
      find.textContaining('跟随部署 · deepseek-v4-flash'),
      findsOneWidget,
      reason:
          '快速模型回落到部署的**廉价档**，不是主档 —— '
          '两者写成一样会让人以为后台杂活也在烧贵的那个',
    );
    expect(
      find.text('自动挑最便宜的'),
      findsOneWidget,
      reason:
          '绘画模型不指派时是自动挑，不是跟随部署 —— '
          '部署那条多半根本不能生图',
    );
  });

  testWidgets('绘画那一行只列画得出来的', (tester) async {
    final c = _boot(_Api());
    addTearDown(c.dispose);
    await _pump(tester, c);
    await _openPicker(tester, '绘画模型');

    expect(
      find.text('Qwen Image Plus'),
      findsOneWidget,
      reason: '它 image_output = true，是这一行唯一该有的候选',
    );
    expect(
      find.text('DeepSeek V4 Pro'),
      findsNothing,
      reason:
          '对话模型出现在绘画候选里的话，用户会挑一个，'
          '然后在保存那一刻吃服务端一个 400，而他看不出哪个能选',
    );
    expect(
      find.text('MiniMax-M2.5'),
      findsNothing,
      reason:
          '目录里查不到的（image_output 是 null）也不列：'
          '「不知道能不能画」不足以当成能画',
    );
  });

  testWidgets('绘画那一行不拿「不支持工具调用」去拦', (tester) async {
    final c = _boot(_Api());
    addTearDown(c.dispose);
    await _pump(tester, c);
    await _openPicker(tester, '绘画模型');

    expect(
      _rowEnabled(tester, 'Qwen Image Plus'),
      isTrue,
      reason:
          '生图模型基本都不支持工具调用。照搬另外两行那条规则的话，'
          '这一行会把它该提供的每一项都画成灰的 —— 一个都选不了',
    );
  });

  testWidgets('主模型那一行仍然拦住不支持工具调用的', (tester) async {
    final c = _boot(_Api());
    addTearDown(c.dispose);
    await _pump(tester, c);
    await _openPicker(tester, '主模型');

    expect(
      _rowEnabled(tester, 'Qwen Image Plus'),
      isFalse,
      reason:
          '把上一条那个放宽误加到这一行的话，用户能把一个生图模型'
          '设成主对话模型 —— 它会流畅地回答而一个工具都不调',
    );
  });

  testWidgets('主模型那一行拦住生图模型 —— 哪怕目录里查不到它', (tester) async {
    final c = _boot(_Api());
    addTearDown(c.dispose);
    await _pump(tester, c);
    await _openPicker(tester, '主模型');

    expect(
      _rowEnabled(tester, 'qwen-image-2.0'),
      isFalse,
      reason:
          '它 tool_call 是 null（目录查不到），光靠「不支持工具调用就拦」'
          '拦不住 —— 而 image_output 明说了它是画画的。选中它每一轮对话'
          '都会失败，错误来自供应商，用户看不出是选错了',
    );
    expect(
      find.textContaining('这是生图模型，对话跑不了'),
      findsWidgets,
      reason:
          '拦下来要说**这一个**为什么不行。写「不支持工具调用」的话，'
          '用户会以为换个别的生图模型就行了',
    );
  });

  testWidgets('绘画那一行照样列得出目录查不到的生图模型', (tester) async {
    final c = _boot(_Api());
    addTearDown(c.dispose);
    await _pump(tester, c);
    await _openPicker(tester, '绘画模型');

    expect(
      _rowEnabled(tester, 'qwen-image-2.0'),
      isTrue,
      reason:
          '上一条那个否决只在要跑 agent 时成立。误加到这一行的话，'
          '绘画模型一个都选不了 —— 真实账号上 20 个 qwen-image 全都是'
          '目录查不到的那一类',
    );
  });

  testWidgets('角色选择器里没有「自动」', (tester) async {
    final c = _boot(_Api());
    addTearDown(c.dispose);
    await _pump(tester, c);
    await _openPicker(tester, '主模型');

    expect(
      find.text('自动'),
      findsNothing,
      reason:
          '角色存的是一对 (来源, 型号)，而「自动」不是任何一条来源上的'
          '型号 —— 摆出来选了也存不进去，服务端会 400',
    );
  });

  testWidgets('保存是整份发的，不会把别的角色清掉', (tester) async {
    final api = _Api(
      initial: const RoleAssignments(
        roles: [
          RoleAssignment(
            role: ModelRole.main,
            source: 'src-a',
            model: 'qwen3-max',
          ),
        ],
      ),
    );
    final c = _boot(api);
    addTearDown(c.dispose);
    await _pump(tester, c);

    // 只改「快速模型」
    await _openPicker(tester, '快速模型');
    await tester.tap(find.text('DeepSeek V4 Pro'));
    await tester.pumpAndSettle();

    expect(api.saves, hasLength(1));
    expect(
      api.saves.single,
      containsAll(<String>['cheap=deepseek-v4-pro', 'main=qwen3-max']),
      reason:
          '服务端那侧是整份替换。只发改的那一条的话，用户改了'
          '「快速模型」而「主模型」被清掉 —— 两件事毫无关联，'
          '他根本不会往这儿想',
    );
  });

  testWidgets('选「不指派」是清掉那个角色，不是选中一个空模型', (tester) async {
    final api = _Api(
      initial: const RoleAssignments(
        roles: [
          RoleAssignment(
            role: ModelRole.main,
            source: 'src-a',
            model: 'qwen3-max',
          ),
          RoleAssignment(
            role: ModelRole.cheap,
            source: 'src-a',
            model: 'qwen3-max',
          ),
        ],
      ),
    );
    final c = _boot(api);
    addTearDown(c.dispose);
    await _pump(tester, c);

    await _openPicker(tester, '主模型');
    await tester.tap(find.text('不指派'));
    await tester.pumpAndSettle();

    expect(
      api.saves.single,
      equals(<String>['cheap=qwen3-max']),
      reason:
          '「不指派」要把这个角色从列表里拿掉（不在列表里 = 没指派），'
          '而另外那个必须原样留着',
    );
  });

  testWidgets('指派的型号不在列表里了要说出来', (tester) async {
    final api = _Api(
      initial: const RoleAssignments(
        roles: [
          RoleAssignment(
            role: ModelRole.main,
            source: 'src-deleted',
            model: '一个已经没了的型号',
          ),
        ],
      ),
    );
    final c = _boot(api);
    addTearDown(c.dispose);
    await _pump(tester, c);

    expect(
      find.textContaining('已经不在可选列表里了'),
      findsOneWidget,
      reason:
          '那条来源被删了或关了，下一轮会静默回落到默认 —— '
          '这一行还显示得好好的话，用户以为自己一直在用指派的那个',
    );
  });

  group('RoleAssignments', () {
    test('改一条不动另外两条', () {
      const before = RoleAssignments(
        roles: [
          RoleAssignment(role: ModelRole.main, source: 's', model: 'a'),
          RoleAssignment(role: ModelRole.cheap, source: 's', model: 'b'),
        ],
      );
      final after = before.with_(
        ModelRole.cheap,
        const RoleAssignment(role: ModelRole.cheap, source: 's', model: 'c'),
      );
      expect(after.of(ModelRole.main)?.model, 'a');
      expect(after.of(ModelRole.cheap)?.model, 'c');
      expect(
        before.of(ModelRole.cheap)?.model,
        'b',
        reason: '要不可变地改：就地改的话，保存失败时没有原件可回滚',
      );
    });

    test('传 null 是清掉那个角色', () {
      const before = RoleAssignments(
        roles: [
          RoleAssignment(role: ModelRole.main, source: 's', model: 'a'),
          RoleAssignment(role: ModelRole.image, source: 's', model: 'b'),
        ],
      );
      final after = before.with_(ModelRole.main, null);
      expect(after.of(ModelRole.main), isNull);
      expect(after.roles, hasLength(1));
    });

    test('认不出的角色读的时候忽略，不让整页失败', () {
      final parsed = RoleAssignments.fromJson({
        'roles': [
          {'role': 'main', 'source': 's', 'model': 'a'},
          {'role': 'translate', 'source': 's', 'model': 'b'},
        ],
      });
      expect(parsed.roles, hasLength(1));
      expect(
        parsed.of(ModelRole.main)?.model,
        'a',
        reason:
            '「translate」是一个比这个客户端新的版本写进去的。'
            '为它整页报错的话，用户连自己配过的那两个都看不到了',
      );
    });

    test('线上写法与服务端那套一致', () {
      // 拼错的表现是静默不生效：存进去了、服务端读出来认不出、
      // 于是回落到部署那个，而这一页上那一栏显示得好好的
      expect(ModelRole.main.wire, 'main');
      expect(ModelRole.cheap.wire, 'cheap');
      expect(ModelRole.image.wire, 'image');
      for (final r in ModelRole.values) {
        expect(ModelRole.parse(r.wire), r);
      }
      expect(ModelRole.parse('translate'), isNull);
      expect(ModelRole.parse('MAIN'), isNull, reason: '大小写不宽容，与服务端同');
    });
  });
}
