part of '../mock_cortex_api.dart';

mixin _SessionsMock on MockCortexApiBase {
  @override
  Future<List<ChatSession>> sessions({
    bool includeArchived = false,
    String? projectId,
  }) async {
    await _latency(180);
    return List.unmodifiable(
      _sessions
          .where((s) => includeArchived || !s.archived)
          .where((s) => projectId == null || s.projectId == projectId),
    );
  }

  @override
  Future<List<SessionSearchHit>> searchSessions(
    String query, {
    bool includeArchived = false,
  }) async {
    await _latency(150);
    final needle = query.trim().toLowerCase();
    // 空词回空，与服务端同一个规矩：`contains('')` 对任何串都为真，
    // 于是清空搜索框会变成「列出全部」
    if (needle.isEmpty) return const [];
    return List.unmodifiable(
      _sessions
          .where((s) => includeArchived || !s.archived)
          .where((s) {
            return s.title.toLowerCase().contains(needle) ||
                (s.preview ?? '').toLowerCase().contains(needle);
          })
          .map((s) {
            final inTitle = s.title.toLowerCase().contains(needle);
            return SessionSearchHit(
              sessionId: s.id,
              title: s.titleIsCustom ? s.title : null,
              archived: s.archived,
              titleMatch: inTitle,
              hitCount: inTitle ? 0 : 1,
              excerpt: inTitle ? null : s.preview,
            );
          }),
    );
  }

  @override
  Future<ChatSession> moveSessionToProject(
    String sessionId,
    String? projectId,
  ) async {
    await _latency(110);
    final index = _sessions.indexWhere((s) => s.id == sessionId);
    if (index == -1) {
      throw CortexApiException('session $sessionId 不存在', statusCode: 404);
    }
    if (projectId != null && !_projects.any((p) => p.id == projectId)) {
      throw CortexApiException('project $projectId 不存在', statusCode: 404);
    }
    final updated = _sessions[index].copyWith(projectId: projectId);
    _sessions[index] = updated;
    return updated;
  }

  @override
  Future<ChatSession> setContainerWorkspace(
    String sessionId,
    String? name,
  ) async {
    await _latency(110);
    final index = _sessions.indexWhere((s) => s.id == sessionId);
    if (index == -1) {
      throw CortexApiException('session $sessionId 不存在', statusCode: 404);
    }
    // 名字的形状**在这里也判一次**，而且用与服务端一模一样的措辞：
    // mock 后端是很多人第一次点这个功能的地方，让它比真后端宽松的话，
    // 界面会在 mock 上放行一个真后端会拒的名字 —— 那种「本地好好的、
    // 一上线就报错」最难查
    if (name != null && !_containerWsShape.hasMatch(name)) {
      throw CortexApiException(
        '工作区名 "$name" 不合格：只允许一段字母数字与 . _ -，'
        '必须以字母或数字开头，长度 1~64',
        statusCode: 400,
      );
    }
    final updated = _sessions[index].copyWith(containerWorkspace: name);
    _sessions[index] = updated;
    return updated;
  }

  /// 与库里那条 CHECK、以及 `cortex-local` 的 `container_subdir` 是同一份规则。
  static final _containerWsShape = RegExp(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$');

  /// Pages the same way the daemon does, cursor and all.
  ///
  /// The fixtures are far too small to need paging — which is exactly why the
  /// mechanism has to be here. If the mock always answered with everything and
  /// `has_more: false`, the "load earlier" path would first execute in
  /// production, and a client bug in it (a cursor never advancing, a page
  /// appended instead of prepended) would be invisible until then.
  ///
  /// Cursor shape copies `cortexd::cursor` (`<RFC3339>|<id>`) so that a client
  /// which accidentally started parsing it would break here too, not only
  /// against a real daemon.
  @override
  Future<SessionDetail> sessionDetail(
    String id, {
    int? limit,
    String? before,
  }) async {
    await _latency(240);
    final index = _sessions.indexWhere((s) => s.id == id);
    if (index == -1) {
      throw CortexApiException('session $id 不存在', statusCode: 404);
    }
    final all =
        MockCortexApiBase._episodes.values
            .where((e) => e.sessionId == id)
            .toList()
          ..sort(_byOccurrence);

    var upTo = all.length;
    if (before != null) {
      final cut = _decodeCursor(before);
      upTo = all.indexWhere((e) => _compareCursor(e, cut) >= 0);
      if (upTo < 0) upTo = all.length;
    }

    final size = (limit ?? 500).clamp(1, 500);
    final start = upTo - size < 0 ? 0 : upTo - size;
    final page = all.sublist(start, upTo);
    final hasMore = start > 0;

    return SessionDetail(
      session: _sessions[index],
      episodes: page,
      hasMore: hasMore,
      // Points at the first row of this page: the next request wants everything
      // strictly older than it.
      nextCursor: hasMore && page.isNotEmpty ? _encodeCursor(page.first) : null,
    );
  }

  static int _byOccurrence(Episode a, Episode b) {
    final t = (a.occurredAt ?? DateTime(0)).compareTo(
      b.occurredAt ?? DateTime(0),
    );
    // Ties broken by id, same as the daemon: fixtures share a timestamp often
    // enough that a timestamp-only order would not be stable.
    return t != 0 ? t : a.id.compareTo(b.id);
  }

  static String _encodeCursor(Episode e) =>
      '${(e.occurredAt ?? DateTime(0)).toUtc().toIso8601String()}|${e.id}';

  static (DateTime, String) _decodeCursor(String raw) {
    final sep = raw.lastIndexOf('|');
    if (sep < 0) {
      throw CortexApiException('游标格式不对，缺少 "|"：$raw', statusCode: 400);
    }
    final when = DateTime.tryParse(raw.substring(0, sep));
    if (when == null) {
      throw CortexApiException('游标里的时间不是合法 RFC3339', statusCode: 400);
    }
    return (when.toLocal(), raw.substring(sep + 1));
  }

  static int _compareCursor(Episode e, (DateTime, String) cut) {
    final t = (e.occurredAt ?? DateTime(0)).compareTo(cut.$1);
    return t != 0 ? t : e.id.compareTo(cut.$2);
  }

  /// Mirrors the daemon's tri-state and, importantly, its **validation**.
  ///
  /// The mock rejects the same shapes for the same stated reasons (relative
  /// path, filesystem root, home directory). It cannot check existence — there
  /// is no filesystem behind it — but if it accepted everything, the error
  /// surface would only ever be exercised in production, which is the exact
  /// failure mode the mock exists to prevent.
  @override
  Future<ChatSession> updateSession(
    String id, {
    String? title,
    bool? archived,
    bool? pinned,
    String? workspace,
    bool clearWorkspace = false,
  }) async {
    await _latency(140);
    final index = _sessions.indexWhere((s) => s.id == id);
    if (index == -1) {
      throw CortexApiException('session $id 不存在', statusCode: 404);
    }
    if (title != null && title.trim().isEmpty) {
      throw const CortexApiException('标题不能为空白', statusCode: 400);
    }
    if (!clearWorkspace && workspace != null) {
      _validateWorkspace(workspace);
    }

    final updated = _sessions[index].copyWith(
      title: title?.trim(),
      titleIsCustom: title != null ? true : null,
      archived: archived,
      pinned: pinned,
      workspace: clearWorkspace
          ? null
          : (workspace == null
                ? _sessions[index].workspace
                : Workspace(root: workspace)),
      updatedAt: DateTime.now(),
    );
    _sessions[index] = updated;
    return updated;
  }

  /// 与服务端同一套判定：空历史 400、`upToEpisodeId` 指错 400、
  /// 截断**含**那条；旧会话原样不动。放松任何一条，错误分支就只会在
  /// 生产上第一次执行 —— 那正是 mock 存在要防的事。
  @override
  Future<ChatSession> forkSession(String id, {String? upToEpisodeId}) async {
    await _latency(240);
    final index = _sessions.indexWhere((s) => s.id == id);
    if (index == -1) {
      throw CortexApiException('session $id 不存在', statusCode: 404);
    }
    var all =
        MockCortexApiBase._episodes.values
            .where((e) => e.sessionId == id)
            .toList()
          ..sort(_byOccurrence);
    if (all.isEmpty) {
      throw const CortexApiException('这个会话还没有任何消息，没有可分叉的历史', statusCode: 400);
    }
    if (upToEpisodeId != null) {
      final at = all.indexWhere((e) => e.id == upToEpisodeId);
      if (at == -1) {
        throw CortexApiException(
          '消息 $upToEpisodeId 不在这个会话里 —— 「从这里分叉」找不到那个「这里」',
          statusCode: 400,
        );
      }
      all = all.sublist(0, at + 1);
    }

    final source = _sessions[index];
    // 掺进 `_episodes.length`：随机数种子是固定的（可复现的 demo），
    // 两个实例各分叉一次会生成同一串随机数 —— 而 `_episodes` 是 static，
    // 同名 id 会互相覆盖
    final newId =
        'ses_fork_${_random.nextInt(1 << 30)}_${MockCortexApiBase._episodes.length}';
    for (var i = 0; i < all.length; i++) {
      final e = all[i];
      final copiedId = '${newId}_ep_$i';
      MockCortexApiBase._episodes[copiedId] = Episode(
        id: copiedId,
        sessionId: newId,
        role: e.role,
        text: e.text,
        occurredAt: e.occurredAt,
        // 附件与工具轨迹按引用带走 —— 服务端复制的是引用行，字节不动
        attachments: e.attachments,
        toolCalls: e.toolCalls,
        models: e.models,
      );
    }
    final forked = ChatSession(
      id: newId,
      title: '${source.title}（分叉）',
      titleIsCustom: true,
      createdAt: DateTime.now(),
      updatedAt: DateTime.now(),
      messageCount: all.length,
      preview: all.last.text,
      // 项目归属跟着走，与服务端一致 —— 落到未分组用户会在项目里找不到它
      projectId: source.projectId,
    );
    _sessions.insert(0, forked);
    return forked;
  }

  static void _validateWorkspace(String raw) {
    final path = raw.trim();
    final isAbsolute =
        path.startsWith('/') || RegExp(r'^[A-Za-z]:[/\\]').hasMatch(path);
    if (!isAbsolute) {
      throw CortexApiException(
        '工作区必须是绝对路径，收到的是 $path；'
        '相对路径会相对于服务端进程的工作目录解析，那不是你以为的位置',
        statusCode: 400,
      );
    }
    final normalised = path
        .replaceAll('\\', '/')
        .replaceAll(RegExp(r'/+$'), '');
    if (normalised.isEmpty || RegExp(r'^[A-Za-z]:$').hasMatch(normalised)) {
      throw CortexApiException(
        '$path 是文件系统根目录，不能作为工作区 —— 「整台机器」不是工作区',
        statusCode: 400,
      );
    }
    const systemRoots = ['/etc', '/usr', '/bin', '/sbin', '/boot', '/sys'];
    final lower = normalised.toLowerCase();
    if (lower.startsWith('c:/windows') ||
        systemRoots.any((r) => lower == r || lower.startsWith('$r/'))) {
      throw CortexApiException(
        '$path 是系统目录，agent 在这里的写操作后果不可回滚',
        statusCode: 400,
      );
    }
  }

  // ------------------------------------------------------------------ health

  @override
  Future<Episode> episode(String id) async {
    await _latency(160);
    final ep = MockCortexApiBase._episodes[id];
    if (ep == null) {
      throw CortexApiException('episode $id 不存在', statusCode: 404);
    }
    return ep;
  }

  // -------------------------------------------------------------------- sync

  /// 导出在 mock 上**真的给一份 NDJSON**，不像上面那条抛 501。
  ///
  /// 因为它导的是 mock 自己就有的东西（会话与消息），给得出来。抛 501 的
  /// 那些是「这个数据源根本没有那个后端」—— 两种情形不该长一个样。
  @override
  Future<Uint8List> exportSessions() async {
    final lines = <String>[
      jsonEncode({
        'type': 'header',
        'format': 'cortex-sessions-ndjson',
        'version': 1,
        'session_count': _sessions.length,
      }),
      for (final s in _sessions)
        jsonEncode({
          'type': 'session',
          'id': s.id,
          'title': s.title,
          'archived': s.archived,
        }),
      jsonEncode({'type': 'footer', 'sessions': _sessions.length}),
    ];
    return Uint8List.fromList(utf8.encode('${lines.join('\n')}\n'));
  }

  // ----------------------------------------------------------- sandbox files
}
