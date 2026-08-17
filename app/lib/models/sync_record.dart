import 'json.dart';

/// One row of `GET /sync?since=`, i.e. one entry of the daemon's `sync_log`.
///
/// The log is a single total order across *all* tables, which is what lets a
/// client replay it blindly: records arrive in commit order, so foreign keys are
/// always satisfied without the client knowing the schema.
class SyncRecord {
  const SyncRecord({
    required this.seq,
    required this.table,
    required this.id,
    this.payload = const {},
  });

  /// Position in the global order. Monotonic, gap-free by commit order.
  final int seq;

  /// Business table the row belongs to, e.g. `episodes`, `facts`.
  final String table;

  /// Primary key of the business row.
  final String id;

  /// The row itself. Not consumed yet — the client has no local store, so the
  /// catch-up currently only reads [table] to decide which panes to refresh.
  final Map<String, dynamic> payload;

  factory SyncRecord.fromJson(Map<String, dynamic> json) => SyncRecord(
    seq: asInt(json['seq']),
    table: asString(json['table']),
    id: asString(json['id']),
    payload: json['payload'] is Map<String, dynamic>
        ? json['payload'] as Map<String, dynamic>
        : const {},
  );
}

/// One page of `GET /sync`.
class SyncPage {
  const SyncPage({
    required this.cursor,
    this.records = const [],
    this.hasMore = false,
  });

  /// The `since` to pass next time. **Always take the next cursor from here**,
  /// never from a WebSocket event — this one is the end of what we actually
  /// received.
  final int cursor;

  final List<SyncRecord> records;

  /// More rows are waiting beyond [cursor]; keep pulling until this is false.
  final bool hasMore;

  static const empty = SyncPage(cursor: 0);

  factory SyncPage.fromJson(Map<String, dynamic> json) => SyncPage(
    cursor: asInt(json['cursor']),
    records: asObjectList(
      json['records'],
    ).map(SyncRecord.fromJson).toList(growable: false),
    hasMore: json['has_more'] == true,
  );
}

/// Which pane a changed table feeds.
///
/// Keeping the mapping here rather than in the controller makes the blast
/// radius of a new table obvious: add it to one of these sets, or it silently
/// refreshes nothing.
abstract final class SyncTables {
  /// Rows that change what the session list / transcripts should show.
  ///
  /// `session_events` / `project_events` 是 2026-08-17 补的，而**服务端一直
  /// 在发它们**（`cortex_store::SyncPayload` 与 `sync_payload::to_json` 两处
  /// 都齐了）—— 缺的只是这一行没把它们算进来。
  ///
  /// 症状正是这份注释上面那句警告说的「silently refreshes nothing」：
  /// 另一台设备上改个会话标题、建或删一个项目，帧到了、游标推进了、
  /// 而这台机器上什么都不动 —— 要等下一次整体重拉才看得见。不报错，
  /// 只是「同步好像有点慢」。
  ///
  /// `episode_tool_calls` 同理：一轮里新落的工具调用属于这段对话。
  ///
  /// `summaries` 留着但**这一侧已经没有这张表**（摘要随记忆去了 Cormex）。
  /// 留着不花钱：这是个字符串集合，多一个不存在的表名只是永远不命中。
  static const conversation = {
    'episodes',
    'episode_tool_calls',
    'session_events',
    'project_events',
    'summaries',
  };
}
