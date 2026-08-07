/// A tool invocation surfaced by the agent loop during a turn.
///
/// ## Why this is not one-per-event
///
/// The wire protocol emits **two** `tool` events per invocation — one when the
/// agent dispatches the call and one when it returns:
///
/// ```text
/// {"type":"tool","name":"read_file","summary":"调用 read_file (path=src/main.rs)"}
/// {"type":"tool","name":"read_file","summary":"read_file 返回 12 行 / 340 字符"}
/// ```
///
/// That split is right on the wire (the first event is what lets the UI show
/// "running" during a slow call), but rendering it literally gives two rows
/// that say almost the same thing. So the two halves are folded into one
/// [ToolCall]: [arguments] from the first, [result] from the second, and
/// `result == null` meaning "still running".
///
/// Kept as a view-model rather than reusing `ChatToolEvent` directly so the
/// conversation can hold a stable list of them after the stream has closed.
class ToolCall {
  const ToolCall({
    required this.name,
    this.arguments,
    this.result,
    this.failed = false,
  });

  /// e.g. `memory_search`.
  final String name;

  /// Compacted call arguments, e.g. `(query=pgvector 索引)`. The daemon already
  /// truncates long values — a `write_file` payload must not be able to blow up
  /// the SSE frame.
  final String? arguments;

  /// One-line outcome, e.g. `返回 12 行 / 340 字符`. Null while the call is in
  /// flight.
  final String? result;

  /// The daemon reported a failure. Surfaced, not hidden: a denied path or a
  /// missing file explains an otherwise puzzling answer.
  final bool failed;

  bool get pending => result == null;

  /// A tool that reaches the filesystem, i.e. one that only works when the
  /// session is bound to a workspace.
  bool get touchesFiles =>
      name == 'read_file' || name == 'write_file' || name == 'list_dir';

  /// The workspace-relative path this call operated on, or null.
  ///
  /// Recovered from the compacted argument string rather than from a structured
  /// field, because the wire contract has none — `ChatEvent::Tool` is
  /// `{ name, summary }` and the daemon renders arguments through
  /// `compact_args` into `(k=v, k=v)`. Parsing a rendered string is not lovely,
  /// but the alternative is showing `write_file` with no indication of *which
  /// file*, which is exactly the thing a user needs to see before the write
  /// lands.
  ///
  /// Two details make this reliable enough in practice:
  ///
  /// * `compact_args` walks a `serde_json::Map`, which is a `BTreeMap` — keys
  ///   come out **sorted**, so in `write_file` the huge `content` value always
  ///   precedes `path`. Taking the *last* `path=` therefore skips a `content`
  ///   that happens to contain the literal text `path=`.
  /// * Long values are truncated by the daemon with a trailing `…`, which is
  ///   stripped here so a shortened path is not mistaken for a real name.
  ///
  /// A structured `ChatEvent::Tool { path: Option<String> }` would retire this;
  /// noted in the report as a proposed contract addition.
  String? get targetPath {
    final args = arguments;
    if (args == null || args.isEmpty) return null;
    final matches = _pathArg.allMatches(args);
    if (matches.isEmpty) return null;
    var value = matches.last.group(1)?.trim() ?? '';
    if (value.endsWith('…')) value = value.substring(0, value.length - 1);
    return value.isEmpty ? null : value;
  }

  /// `path=` at the start of the argument list or after a separator. Anchoring
  /// on the separator is what stops a `content` value that mentions `path=`
  /// from matching.
  static final RegExp _pathArg = RegExp(r'(?:^|[(,]\s*)path=([^,)]*)');

  ToolCall copyWith({String? arguments, String? result, bool? failed}) =>
      ToolCall(
        name: name,
        arguments: arguments ?? this.arguments,
        result: result ?? this.result,
        failed: failed ?? this.failed,
      );

  /// Folds one `tool` event into [calls], returning a new list.
  ///
  /// Pairing rule: an event whose name matches the **last** entry and which is
  /// still awaiting a result closes that entry; anything else opens a new one.
  /// This is exactly the agent loop's ordering — it dispatches a call, awaits
  /// it, emits the result, then moves to the next call — so no correlation id
  /// is needed. Two consecutive calls to the same tool still produce two rows,
  /// because the first one is no longer pending by then.
  static List<ToolCall> merge(
    List<ToolCall> calls,
    String name,
    String? summary,
  ) {
    if (calls.isNotEmpty && calls.last.name == name && calls.last.pending) {
      final result = _stripPrefix(summary, '$name ');
      return [
        ...calls.take(calls.length - 1),
        calls.last.copyWith(
          result: result ?? '已完成',
          failed: result != null && result.startsWith('失败'),
        ),
      ];
    }
    return [
      ...calls,
      ToolCall(name: name, arguments: _stripPrefix(summary, '调用 $name ')),
    ];
  }

  /// The daemon inlines the tool name in both summaries because the CLI prints
  /// them as standalone lines. Paired into one row it is redundant, so it goes.
  /// A summary that does not carry the prefix is passed through untouched.
  static String? _stripPrefix(String? summary, String prefix) {
    if (summary == null || summary.isEmpty) return null;
    final rest = summary.startsWith(prefix)
        ? summary.substring(prefix.length).trim()
        : summary.trim();
    return rest.isEmpty ? null : rest;
  }
}
