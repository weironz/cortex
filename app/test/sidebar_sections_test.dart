/// 左栏下半部那三段：项目 / Pinned / 聊天。
///
/// # 这一组盯住的头号失败：**同一条会话在左栏出现两次**
///
/// 三段各自有自己的取数条件，谁也不知道别人取走了什么。漏一个排除条件，
/// 一条置顶的、又属于置顶项目的会话就会画两行 —— 点哪一行都对，
/// 界面也不会报错，而用户会以为自己看重了，或者以为有两条一模一样的对话。
///
/// 其余：段头的条数要与眼前那些行对得上、空段整个不画、折叠状态存的是
/// 「折起来的那些」而不是「展开的那些」（反过来存的话，新用户一进来
/// 三段全是折的，左栏一片空白）。
library;

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/sessions/session_list.dart';
import 'package:cortex_app/models/chat_session.dart';
import 'package:cortex_app/models/project.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/project_controller.dart';
import 'package:cortex_app/state/sidebar_sections.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

/// 一份写死的会话与项目 —— 三段的分法只跟这两份数据有关。
class _FixedApi extends MockCortexApi {
  _FixedApi({
    required List<ChatSession> sessions,
    required List<Project> projects,
  }) : _sessionRows = sessions,
       _projectRows = projects,
       super(instant: true);

  // 字段名不能与被覆写的那两个方法撞名，否则 Dart 直接拒编
  final List<ChatSession> _sessionRows;
  final List<Project> _projectRows;

  @override
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) async => _sessionRows;

  @override
  Future<List<Project>> projects() async => _projectRows;
}

final Map<String, String> _written = {};

ProviderContainer _boot({
  required List<ChatSession> sessions,
  required List<Project> projects,
  Map<String, String> saved = const {},
}) => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_MockConfig.new),
    cortexApiProvider.overrideWithValue(
      _FixedApi(sessions: sessions, projects: projects),
    ),
    settingsReaderProvider.overrideWithValue(() async => saved),
    settingsWriterProvider.overrideWithValue((_) async {}),
    settingsPatcherProvider.overrideWithValue((k, v) async {
      _written[k] = v;
    }),
  ],
);

ChatSession _s(String id, {bool pinned = false, String? project}) =>
    ChatSession(id: id, title: id, pinned: pinned, projectId: project);

Project _p(String id, {bool pinned = false}) =>
    Project(id: id, name: id, pinned: pinned);

Future<void> _pump(WidgetTester tester, ProviderContainer c) async {
  tester.view.physicalSize = const Size(320, 1400);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: c,
      child: const MaterialApp(home: Scaffold(body: SessionList())),
    ),
  );
  for (var i = 0; i < 14; i++) {
    await tester.pump(const Duration(milliseconds: 80));
  }
}

void main() {
  setUp(_written.clear);

  group('折叠状态存的是折起来的那些', () {
    test('什么都没存过 = 三段全展开', () {
      final c = _boot(sessions: const [], projects: const []);
      addTearDown(c.dispose);
      expect(
        c.read(sidebarSectionsProvider),
        isEmpty,
        reason:
            '⚠️ 反过来存（存展开的那些）的话，一个还没存过任何东西的新用户'
            '会被读成「三段全折起来」，左栏一片空白',
      );
    });

    test('折起来一段之后，落盘的是它的 key', () async {
      final c = _boot(sessions: const [], projects: const []);
      addTearDown(c.dispose);
      c.read(sidebarSectionsProvider.notifier).toggle(SidebarSection.chats);
      expect(c.read(sidebarSectionsProvider), {SidebarSection.chats});
      expect(_written['sidebar_collapsed_sections'], 'chats');

      c.read(sidebarSectionsProvider.notifier).toggle(SidebarSection.chats);
      expect(_written['sidebar_collapsed_sections'], '');
    });

    test('落盘用的 key 与 Dart 的枚举名分开', () {
      // 两者撞在一起时这条测试不会红，但它把「改枚举名 = 清空所有人的
      // 折叠状态」这件事写在了眼前
      for (final s in SidebarSection.values) {
        expect(s.key, isNotEmpty);
      }
      expect(SidebarSection.pinned.key, 'pinned');
      expect(SidebarSection.pinned.label, 'Pinned');
    });
  });

  group('三段的分法', () {
    testWidgets('⚠️ 一条会话只出现在一段里', (tester) async {
      // 最坏的那一条：既置顶、又属于一个置顶的项目
      final c = _boot(
        sessions: [
          _s('both', pinned: true, project: 'P'),
          _s('inproject', project: 'P'),
          _s('plain'),
        ],
        projects: [_p('P', pinned: true)],
      );
      addTearDown(c.dispose);
      await _pump(tester, c);

      expect(
        find.text('both'),
        findsOneWidget,
        reason:
            '⚠️ 两行的话点哪一行都对，界面也不报错 —— '
            '而用户会以为自己看重了，或者以为有两条一模一样的对话',
      );
      expect(find.text('inproject'), findsOneWidget);
      expect(find.text('plain'), findsOneWidget);
    });

    testWidgets('置顶的会话进 Pinned，不留在聊天段', (tester) async {
      final c = _boot(
        sessions: [_s('pinned-one', pinned: true), _s('plain')],
        projects: const [],
      );
      addTearDown(c.dispose);
      await _pump(tester, c);

      expect(find.text('Pinned'), findsOneWidget);
      expect(find.text('聊天'), findsOneWidget);
      expect(find.text('pinned-one'), findsOneWidget);
    });

    testWidgets('空的段整个不画', (tester) async {
      final c = _boot(
        sessions: [_s('plain')],
        // 有项目但一个都没置顶 → 「项目」那一段是空的
        projects: [_p('P')],
      );
      addTearDown(c.dispose);
      await _pump(tester, c);

      expect(
        find.text('项目'),
        findsNothing,
        reason: '一个常年空着的段头只是噪音，而它下面什么都没有这件事，标题本身也说不清楚',
      );
      expect(find.text('Pinned'), findsNothing);
    });

    testWidgets('折起来之后那一段的行就不画了', (tester) async {
      final c = _boot(
        sessions: [_s('pinned-one', pinned: true)],
        projects: const [],
      );
      addTearDown(c.dispose);
      await _pump(tester, c);
      expect(find.text('pinned-one'), findsOneWidget);

      await tester.tap(find.byKey(const ValueKey('section:pinned')));
      for (var i = 0; i < 6; i++) {
        await tester.pump(const Duration(milliseconds: 80));
      }

      expect(find.text('pinned-one'), findsNothing);
      expect(find.text('Pinned'), findsOneWidget, reason: '段头还在，否则就没法再展开了');
    });

    testWidgets('一段都用不上时退回一张平铺列表', (tester) async {
      final c = _boot(sessions: [_s('a'), _s('b')], projects: const []);
      addTearDown(c.dispose);
      await _pump(tester, c);

      expect(
        find.text('聊天'),
        findsNothing,
        reason: '三个段头对着一列会话，每一个都不传达信息，只是白占三行',
      );
      expect(find.text('a'), findsOneWidget);
      expect(find.text('b'), findsOneWidget);
    });
  });

  group('置顶要核对服务端真的改了', () {
    test('⚠️ 老服务端回 400 时也要说话，不能静默', () async {
      final api = _RejectingApi();
      final c = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_MockConfig.new),
          cortexApiProvider.overrideWithValue(api),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
          settingsWriterProvider.overrideWithValue((_) async {}),
        ],
      );
      addTearDown(c.dispose);

      final said = await c
          .read(projectControllerProvider.notifier)
          .setPinned('p1', true);

      expect(
        said,
        contains('不支持'),
        reason:
            '⚠️ 2026-08-23 在 0.1.18 的生产上实测：老服务端回 400，'
            '而那个异常一路逃进按钮的回调 —— 界面上的表现是**点了完全没反应**',
      );
    });

    test('⚠️ 老服务端静默忽略 pinned 时，说实话而不是假装成功', () async {
      final api = _DeafApi();
      final c = ProviderContainer(
        overrides: [
          appConfigProvider.overrideWith(_MockConfig.new),
          cortexApiProvider.overrideWithValue(api),
          settingsReaderProvider.overrideWithValue(
            () async => const <String, String>{},
          ),
          settingsWriterProvider.overrideWithValue((_) async {}),
        ],
      );
      addTearDown(c.dispose);
      c.listen(chatControllerProvider, (_, _) {}, fireImmediately: true);
      for (var i = 0; i < 20; i++) {
        await Future<void>.delayed(const Duration(milliseconds: 10));
        if (!c.read(chatControllerProvider).sessionsLoading) break;
      }

      final said = await c
          .read(chatControllerProvider.notifier)
          .setPinned('s1', true);

      expect(
        said,
        contains('不支持'),
        reason:
            '⚠️ 老服务端**不报错**，只是静默忽略这个字段并回 200。'
            '不核对的话界面上那一行当场搬进 Pinned 段、刷新又弹回来 —— '
            '用户看到的是「点了一下，它抖了一下又回来了」',
      );
      expect(
        c.read(chatControllerProvider).sessions.first.pinned,
        isFalse,
        reason: '而且不能把那条「没变」的记录当成成功写回状态',
      );
    });
  });
}

/// 一个**听不懂 `pinned`** 的老服务端：照收请求、回 200，字段原样不动。
class _DeafApi extends MockCortexApi {
  _DeafApi() : super(instant: true);

  final _stored = [_s('s1')];

  @override
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) async => _stored;

  @override
  Future<ChatSession> updateSession(
    String id, {
    String? title,
    bool? archived,
    bool? pinned,
    String? workspace,
    bool clearWorkspace = false,
  }) async {
    // 这就是老服务端做的事：pinned 那个键它压根不认识
    final i = _stored.indexWhere((s) => s.id == id);
    if (i < 0) throw CortexApiException('没有这条', statusCode: 404);
    return _stored[i];
  }
}

/// 一个**不认识 `pinned`** 的老服务端：只带 pinned 的 PATCH 回 400
///（真实的 0.1.18 就是这么答的：「请求体里没有任何要改的字段（name）」）。
class _RejectingApi extends MockCortexApi {
  _RejectingApi() : super(instant: true);

  @override
  Future<Project> patchProject(String id, {String? name, bool? pinned}) async {
    if (name == null) {
      throw const CortexApiException('请求体里没有任何要改的字段（name）', statusCode: 400);
    }
    return Project(id: id, name: name);
  }
}
