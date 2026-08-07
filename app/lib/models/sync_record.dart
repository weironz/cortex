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
    records: asObjectList(json['records'])
        .map(SyncRecord.fromJson)
        .toList(growable: false),
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
  static const conversation = {'episodes', 'summaries'};

  /// Rows that change what the memory pane should show.
  static const memory = {
    'facts',
    'fact_events',
    'entities',
    'entity_merges',
    'redactions',
  };
}
