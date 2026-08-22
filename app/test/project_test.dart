/// 项目 —— 会话的分组容器。
///
/// 这一组测试盯住四件会静默出错的事：
///
/// 1. 契约字段读错（`session_count` 缺失时不能变成 null，`project_id` 缺失
///    时必须读成「未分组」）
/// 2. 项目被删之后，指向它的会话必须落回未分组而**不是从侧边栏消失**
/// 3. 换后端时被掐断的请求，不许把错误写回一个刚清干净的界面
/// 4. 删除确认里那句话 —— 用户在这里唯一真正害怕的是「删项目把对话也删了」
library;

import 'dart:async';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/models/image_prefs.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/app.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/chat_session.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/models/project.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/project_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

// ---------------------------------------------------------------- 测试替身

/// 一个把项目存在内存里的替身，没有 [MockCortexApi] 那套刻意的延迟。
/// 像真服务端那样：删一个不存在的项目回 404，而不是静默成功。
///
/// 原来的替身在这里是**无声的**（`removeWhere` 删不到就当没事），
/// 于是「404 被当成不支持」这条从来没被测到过。
class _GoneOnDelete extends _Api {
  _GoneOnDelete({super.seed});

  @override
  Future<void> deleteProject(String id) async {
    if (!stored.any((p) => p.id == id)) {
      throw CortexApiException('项目不存在', statusCode: 404);
    }
    stored.removeWhere((p) => p.id == id);
  }
}

class _Api extends MockCortexApi {
  _Api({List<Project>? seed}) : stored = [...?seed];

  final List<Project> stored;
  int loads = 0;
  final List<(String, String?)> moves = [];

  /// 第一次 `chat()` 之前有没有 PATCH 过分组。**顺序是这次要钉的东西。**
  bool movedBeforeFirstChat = false;
  bool _chatted = false;

  @override
  Future<List<Project>> projects() async {
    loads++;
    return List.of(stored);
  }

  @override
  Future<Project> createProject(String name) async {
    final p = Project(id: 'prj_${stored.length + 1}', name: name);
    stored.add(p);
    return p;
  }

  @override
  Future<Project> patchProject(String id, {String? name, bool? pinned}) async {
    final i = stored.indexWhere((p) => p.id == id);
    stored[i] = stored[i].copyWith(name: name, pinned: pinned);
    return stored[i];
  }

  @override
  Future<void> deleteProject(String id) async {
    stored.removeWhere((p) => p.id == id);
  }

  @override
  Stream<ChatEvent> chat({
    required String sessionId,
    required String message,
    List<Attachment> attachments = const [],
    PermissionMode permissionMode = PermissionMode.ask,
    String? model,
    String? source,
    ImagePrefs? imagePrefs,
  }) {
    _chatted = true;
    return super.chat(
      sessionId: sessionId,
      message: message,
      attachments: attachments,
      permissionMode: permissionMode,
    );
  }

  @override
  Future<ChatSession> moveSessionToProject(
    String sessionId,
    String? projectId,
  ) async {
    if (!_chatted) movedBeforeFirstChat = true;
    moves.add((sessionId, projectId));
    return ChatSession(id: sessionId, title: 't', projectId: projectId);
  }

  @override
  Future<ChatSession> setContainerWorkspace(String sessionId, String? name) =>
      throw UnimplementedError();

  @override
  Future<void> stopRun(String sessionId) => throw UnimplementedError();
}

/// 老服务端：`/projects` 这条路由根本不存在。
class _NoProjects extends MockCortexApi {
  int calls = 0;

  @override
  Future<List<Project>> projects() async {
    calls++;
    throw const CortexApiException('Not Found', statusCode: 404);
  }
}

ProviderContainer _boot(MockCortexApi api) =>
    ProviderContainer(overrides: [cortexApiProvider.overrideWithValue(api)]);

/// 让排在微任务里的 `load()` 跑完。
Future<void> _settle([int turns = 4]) async {
  for (var i = 0; i < turns; i++) {
    await Future<void>.delayed(Duration.zero);
  }
}

ChatSession _session(String id, {String? projectId}) =>
    ChatSession(id: id, title: id, projectId: projectId);

void main() {
  group('Project 模型', () {
    test('JSON 往返不丢字段', () {
      final json = {
        'id': 'prj_1',
        'name': 'Cortex 客户端',
        'created_at': '2026-08-01T10:30:00Z',
        'session_count': 7,
      };

      final project = Project.fromJson(json);
      expect(project.id, 'prj_1');
      expect(project.name, 'Cortex 客户端');
      expect(project.sessionCount, 7);
      expect(project.createdAt?.toUtc(), DateTime.utc(2026, 8, 1, 10, 30));

      expect(
        Project.fromJson(project.toJson()),
        project,
        reason:
            '往返回来必须还是同一个项目。这里最容易丢的是时区：'
            'fromJson 解出来的是本地时间，toJson 不转回 UTC 的话，'
            '第二次解析会把偏移再算一遍',
      );
    });

    test('缺字段的老服务端读成「空项目」，而不是抛异常', () {
      final project = Project.fromJson({'id': 'prj_1'});
      expect(project.name, '未命名项目');
      expect(
        project.sessionCount,
        0,
        reason:
            '这个数会出现在删除确认里。读成 null 再在界面上 `?? 0`，'
            '就变成每处都要记得兜底，而漏掉的那处会打印 "null 个会话"',
      );
      expect(project.createdAt, isNull);
    });

    test('会话缺 project_id 时是未分组，不是异常', () {
      final s = ChatSession.fromJson({'id': 's1', 'title': 't'});
      expect(
        s.projectId,
        isNull,
        reason:
            '没有项目功能的部署里每条会话都是这个形态；'
            '这里抛异常等于整张会话列表拉不出来',
      );
      expect(
        ChatSession.fromJson({
          'id': 's1',
          'title': 't',
          'project_id': 'prj_1',
        }).projectId,
        'prj_1',
      );
    });

    test('copyWith(projectId: null) 是「移出项目」，不是「别动」', () {
      final grouped = _session('s1', projectId: 'prj_1');
      expect(
        grouped.copyWith(projectId: null).projectId,
        isNull,
        reason:
            '显式传 null 就是移出。当成「没传」处理的话，'
            '「移出项目」这个动作在界面上永远没有反应',
      );
      expect(
        grouped.copyWith(title: '改个名').projectId,
        'prj_1',
        reason: '不传就不该动分组 —— 否则每次改名都会把会话踢出项目',
      );
    });
  });

  group('按项目分组', () {
    final projects = [
      const Project(id: 'p1', name: 'A'),
      const Project(id: 'p2', name: 'B'),
    ];

    test('未分组永远排在最后，空项目也留一行', () {
      final groups = groupSessionsByProject([
        _session('s1', projectId: 'p1'),
        _session('s2'),
      ], projects);

      expect(groups.map((g) => g.projectId), ['p1', 'p2', null]);
      expect(
        groups[1].sessions,
        isEmpty,
        reason:
            '刚建好的项目一条会话都没有。不给它一行的话，'
            '「新建项目」看起来什么也没发生',
      );
      expect(groups.last.isUngrouped, isTrue);
    });

    test('指向已经不存在的项目 = 未分组，会话不许消失', () {
      final groups = groupSessionsByProject([
        _session('s1', projectId: 'p1'),
        // 项目刚被删掉，手上这份数据还指着那个 id
        _session('s2', projectId: 'deleted'),
      ], projects);

      expect(
        groups.last.sessions.map((s) => s.id),
        ['s2'],
        reason:
            '删项目到下一次 GET /sessions 之间就是这个形态。'
            '按「找不到就丢掉」处理的话，这段时间里那些会话会从侧边栏整个'
            '消失 —— 而用户刚刚才被告知「会话不会丢」',
      );
    });
  });

  group('ProjectController', () {
    test('起来就拉一次列表', () async {
      final api = _Api(
        seed: [const Project(id: 'p1', name: 'A')],
      );
      final container = _boot(api);
      addTearDown(container.dispose);

      container.read(projectControllerProvider);
      await _settle();

      final state = container.read(projectControllerProvider);
      expect(state.projects.map((p) => p.name), ['A']);
      expect(state.loading, isFalse);
      expect(state.error, isNull);
      expect(state.showGrouping, isTrue);
    });

    test('新建之后立刻在列表里，并被选中', () async {
      final container = _boot(_Api());
      addTearDown(container.dispose);
      final controller = container.read(projectControllerProvider.notifier);
      await _settle();

      final created = await controller.create('  新项目  ');

      expect(created?.name, '新项目', reason: '首尾空格要在发出去之前就修掉');
      final state = container.read(projectControllerProvider);
      expect(state.projects.map((p) => p.name), ['新项目']);
      expect(
        state.selectedId,
        created?.id,
        reason:
            '刚建的项目就是用户接下来要用的那个；不选中它，'
            '他得再点一次才能往里放会话',
      );
    });

    test('空白名不发请求', () async {
      final api = _Api();
      final container = _boot(api);
      addTearDown(container.dispose);
      await _settle();

      expect(
        await container.read(projectControllerProvider.notifier).create('   '),
        isNull,
      );
      expect(api.stored, isEmpty, reason: '空白名只会在服务端换来一个 400');
    });

    test('改名就地生效，不必等重新拉列表', () async {
      final api = _Api(
        seed: [const Project(id: 'p1', name: '旧名')],
      );
      final container = _boot(api);
      addTearDown(container.dispose);
      final controller = container.read(projectControllerProvider.notifier);
      await _settle();

      await controller.rename('p1', '新名');

      expect(
        container.read(projectControllerProvider).projects.single.name,
        '新名',
      );
      expect(api.stored.single.name, '新名');
    });

    test('删除只拿掉分组，选中态与折叠态一并清干净', () async {
      final api = _Api(
        seed: [
          const Project(id: 'p1', name: 'A'),
          const Project(id: 'p2', name: 'B'),
        ],
      );
      final container = _boot(api);
      addTearDown(container.dispose);
      final controller = container.read(projectControllerProvider.notifier);
      await _settle();

      controller.select('p1');
      controller.toggleCollapsed('p1');
      await controller.remove('p1');

      final state = container.read(projectControllerProvider);
      expect(state.projects.map((p) => p.id), ['p2']);
      expect(
        state.selectedId,
        isNull,
        reason:
            '留着一个已删项目的 id，「在这个项目里新建会话」'
            '会把会话丢进一个不存在的 id',
      );
      expect(
        state.isCollapsed('p1'),
        isFalse,
        reason: '折叠集合不清理的话，将来复用同一个 id 的项目会莫名其妙是收起的',
      );
    });

    test('删一个已经不在的项目，不许把整个分组功能当成「服务端不支持」', () async {
      // 两台设备各删一次是常有的：桌面端删完，CLI 那边的列表还是旧的。
      // 服务端对第二次删回 404 —— 而 404 同时是「老服务端没有 /projects」的
      // 信号。两件事共用一个状态码，客户端不能只看数字。
      final api = _GoneOnDelete(
        seed: [const Project(id: 'p1', name: 'A')],
      );
      final container = _boot(api);
      addTearDown(container.dispose);
      final controller = container.read(projectControllerProvider.notifier);
      await _settle();

      // 另一台设备先删掉了它，我们手上的列表还是旧的
      api.stored.clear();
      await controller.remove('p1');

      final state = container.read(projectControllerProvider);
      expect(
        state.unsupported,
        isFalse,
        reason:
            '把它当成「这个后端没有项目功能」，整条分组界面会当场消失，'
            '而且再也回不来 —— 用户只是删了两次而已',
      );
      expect(
        state.projects.map((p) => p.id),
        isEmpty,
        reason: '它本来就该没了：服务端说不在，本地也不该留着',
      );
      expect(state.error, isNull, reason: '想要的结果已经达成了（那个项目不在了），没有可报的失败');
    });

    test('重新拉到的列表里没有选中的那个项目，就把选中态放掉', () async {
      final api = _Api(
        seed: [const Project(id: 'p1', name: 'A')],
      );
      final container = _boot(api);
      addTearDown(container.dispose);
      final controller = container.read(projectControllerProvider.notifier);
      await _settle();

      controller.select('p1');
      api.stored.clear(); // 别的设备删掉了它
      await controller.load();

      expect(
        container.read(projectControllerProvider).selectedId,
        isNull,
        reason:
            '这条路径与本机删除不同：本机删除走 remove()，'
            '而别的设备删掉只能靠下一次 load() 发现',
      );
    });
  });

  group('老服务端没有 /projects', () {
    test('404 降级成「没有项目功能」，而不是一条消不掉的报错', () async {
      final container = _boot(_NoProjects());
      addTearDown(container.dispose);

      container.read(projectControllerProvider);
      await _settle();

      final state = container.read(projectControllerProvider);
      expect(state.unsupported, isTrue);
      expect(
        state.error,
        isNull,
        reason:
            '把 404 画成错误，用户看到的是一条自己无论如何都消不掉的红字 —— '
            '而会话列表本身好好的，他什么也不用做',
      );
      expect(state.showGrouping, isFalse, reason: '分组这层界面该安静地不存在');
      expect(state.loading, isFalse);
    });
  });

  group('换后端', () {
    test('被掐断的那个请求，不许把错误写回界面', () async {
      final first = _Hanging();
      final second = _Api();
      var swapped = false;
      final container = ProviderContainer(
        overrides: [
          cortexApiProvider.overrideWith((ref) => swapped ? second : first),
        ],
      );
      addTearDown(container.dispose);

      container.read(projectControllerProvider);
      await _settle();
      expect(container.read(projectControllerProvider).loading, isTrue);

      // 本地 agent 就绪 → provider 重建 → 旧 HttpCortexApi 被 dispose，
      // 它的 dispose 是 _client.close()，在飞的请求当场断掉
      swapped = true;
      container.invalidate(cortexApiProvider);
      container.read(cortexApiProvider);
      await _settle();

      first.completer.completeError(
        const CortexApiException(
          'Connection closed before full header was received',
        ),
      );
      await _settle();

      final state = container.read(projectControllerProvider);
      expect(
        state.error,
        isNull,
        reason:
            '那个请求是我们自己掐断的，不是服务端拉不到项目。'
            '把它的异常写进界面，用户看到的是一个他既看不懂也处理不了的错误，'
            '而唯一的出路是手点刷新',
      );
      expect(state.unsupported, isFalse, reason: '更糟的一种：把尸体当成 404，整个分组界面会当场消失');
      expect(second.loads, greaterThan(0), reason: '换过去要自己重拉一次，不该由用户去点刷新');
    });

    test('折叠状态是视图偏好，换后端不重置', () async {
      final first = _Api(
        seed: [const Project(id: 'p1', name: 'A')],
      );
      final second = _Api(
        seed: [const Project(id: 'p1', name: 'A')],
      );
      var swapped = false;
      final container = ProviderContainer(
        overrides: [
          cortexApiProvider.overrideWith((ref) => swapped ? second : first),
        ],
      );
      addTearDown(container.dispose);

      container.read(projectControllerProvider.notifier).toggleCollapsed('p1');
      await _settle();

      swapped = true;
      container.invalidate(cortexApiProvider);
      container.read(cortexApiProvider);
      await _settle();

      expect(
        container.read(projectControllerProvider).isCollapsed('p1'),
        isTrue,
        reason:
            '收起来是用户的选择，不是后端给的数据。'
            '跟着一起清掉的话，冷启动时那次自动换后端会把所有分组重新展开',
      );
    });
  });

  group('新建在项目里的会话', () {
    /// **分组必须在第一轮之前落到服务端。**
    ///
    /// 沙箱容器与工作区卷按「owner + 项目」分，而服务端是在收到 `/chat`
    /// 的那一刻去库里读这个会话属于哪个项目。补晚一步，第一轮就落进
    /// **未分组**那个容器 —— 所有没分组的会话共用它。
    ///
    /// 真机上的症状：新建项目、发第一句让它写个文件，文件进了默认沙箱；
    /// 第二轮起才进项目沙箱，于是第一轮写的东西再也看不见。生产上留下的
    /// 证据是同一个 owner 名下并排两套容器与两个卷。
    ///
    /// 断言的是**顺序**，不是「最终补上了」—— 后者原来那版也满足。
    test('分组先落地，再发第一句', () async {
      final api = _Api(
        seed: const [Project(id: 'prj_1', name: 'P')],
      );
      final container = _boot(api);
      addTearDown(container.dispose);
      final chat = container.read(chatControllerProvider.notifier);
      await _settle(8);

      final sid = chat.createSession(projectId: 'prj_1');
      expect(api.moves, isEmpty, reason: '还没发消息，不该先建一行状态出来');

      await chat.send('写个文件');
      await _settle(8);

      expect(
        api.moves,
        contains((sid, 'prj_1')),
        reason: '第一轮之前必须已经 PATCH 过分组',
      );
      expect(
        api.movedBeforeFirstChat,
        isTrue,
        reason:
            '顺序才是这条测试的全部：补在 /chat 之后的话，第一轮已经带着'
            '「未分组」去要过钥匙了，文件落在另一个卷里',
      );
    });
  });

  group('移动会话', () {
    test('移出项目发的是显式的 null', () async {
      final api = _Api();
      final container = _boot(api);
      addTearDown(container.dispose);
      final chat = container.read(chatControllerProvider.notifier);
      await _settle(8);

      await chat.moveSessionToProject('ses_01JQZ8K3M9', null);

      expect(
        api.moves,
        [('ses_01JQZ8K3M9', null)],
        reason:
            '「移出」与「别动分组」在 PATCH 请求体里是两件事：'
            '前者要发 project_id: null，后者是根本不带这个字段',
      );
    });
  });

  group('删除项目的确认文案', () {
    test('明说会话不会丢，并给出会变成未分组的条数', () {
      const project = Project(id: 'p1', name: '季度规划', sessionCount: 3);
      final text = deleteProjectWarning(project);

      expect(
        text,
        contains('不会被删除'),
        reason:
            '用户在这里唯一真正害怕的事就是「删项目把对话也删了」，'
            '而那恰恰是唯一不会发生的事。这句话说不清楚，代价不是一次误操作，'
            '是从此没人敢碰这个功能',
      );
      expect(text, contains('未分组'), reason: '要说清那些会话去了哪里');
      expect(text, contains('3'), reason: '给出条数，用户才能核对是不是删对了');
      expect(text, contains('季度规划'), reason: '连着两个项目并排放着，不写名字就分不清删的是哪个');
    });

    test('空项目不说「0 个会话」', () {
      const project = Project(id: 'p1', name: '空的');
      expect(
        deleteProjectWarning(project),
        isNot(contains('0 个会话')),
        reason: '一个空项目上写「里面的 0 个会话不会被删除」既拗口又像是出错了',
      );
    });
  });

  group('侧边栏', () {
    Future<void> boot(WidgetTester tester) async {
      tester.view.physicalSize = const Size(1600, 1000);
      tester.view.devicePixelRatio = 1.0;
      addTearDown(tester.view.reset);

      await tester.pumpWidget(
        ProviderScope(
          overrides: [appConfigProvider.overrideWith(_MockConfig.new)],
          child: const CortexApp(),
        ),
      );
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 900));
    }

    testWidgets('会话按项目分组，未分组单独一组', (tester) async {
      await boot(tester);

      expect(find.text('Cortex 客户端'), findsOneWidget);
      expect(find.text('季度规划'), findsOneWidget);
      expect(
        find.text('未分组'),
        findsOneWidget,
        reason:
            '夹具里那条没有 project_id 的会话必须有地方落脚，'
            '否则它在侧边栏里就是不存在的',
      );
    });

    testWidgets('删除项目的确认框写明会话不会丢，删完会话真的还在', (tester) async {
      await boot(tester);

      await tester.tap(find.byTooltip('「季度规划」的更多操作'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('删除项目').last);
      await tester.pumpAndSettle();

      // 限定在对话框里找：「未分组」在侧边栏上本来就有一个分组标题，
      // 不限定的话这条断言在文案被删光时反而会因为那个标题继续通过
      final inDialog = find.descendant(
        of: find.byType(AlertDialog),
        matching: find.textContaining('不会被删除'),
      );
      expect(inDialog, findsOneWidget);
      expect(
        find.descendant(
          of: find.byType(AlertDialog),
          matching: find.textContaining('未分组'),
        ),
        findsOneWidget,
        reason:
            '确认框必须当场回答「那些对话去哪了」，'
            '而不是让用户点下去之后自己找',
      );

      // 「删除项目」此刻同时是菜单项与确认按钮的文案，取对话框里那个
      await tester.tap(
        find.descendant(
          of: find.byType(AlertDialog),
          matching: find.widgetWithText(FilledButton, '删除项目'),
        ),
      );
      await tester.pumpAndSettle();

      expect(find.text('季度规划'), findsNothing, reason: '分组本身没了');
      expect(
        find.text('Q3 OKR 草稿与部门对齐'),
        findsOneWidget,
        reason:
            '那条会话在这个项目里。它必须还在侧边栏上 —— '
            '这正是确认框刚刚承诺过的事',
      );
    });

    testWidgets('项目分组标题上的 + 建出来的会话属于该项目', (tester) async {
      await boot(tester);

      await tester.tap(find.byTooltip('在「季度规划」里新建会话'));
      await tester.pumpAndSettle();

      final container = ProviderScope.containerOf(
        tester.element(find.byType(CortexApp)),
      );
      final state = container.read(chatControllerProvider);
      final created = state.sessions.firstWhere(
        (s) => s.id == state.activeSessionId,
      );
      expect(
        created.projectId,
        'prj_office',
        reason:
            '在某个项目的标题行上点 +，建出来的会话就该落在那个项目里；'
            '否则用户要再走一次「移动到项目」，而他刚刚已经说过要放哪了',
      );
    });
  });
}

/// 一个「请求发出去就再也不回来」的替身 —— 复现 `_client.close()` 之前的那一刻。
class _Hanging extends MockCortexApi {
  final completer = Completer<List<Project>>();

  @override
  Future<List<Project>> projects() => completer.future;
}

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}
