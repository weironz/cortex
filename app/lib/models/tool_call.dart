import 'chat_event.dart';
import 'json.dart';

/// A tool invocation surfaced by the agent loop during a turn.
///
/// ## Why this is not one-per-event
///
/// The wire protocol emits **two** `tool` events per invocation — one when the
/// agent dispatches the call and one when it returns:
///
/// ```text
/// {"type":"tool","name":"read_file","summary":"调用 read_file (path=src/main.rs)","path":"src/main.rs"}
/// {"type":"tool","name":"read_file","summary":"read_file 返回 12 行 / 340 字符","path":"src/main.rs"}
/// ```
///
/// That split is right on the wire (the first event is what lets the UI show
/// "running" during a slow call), but rendering it literally gives two rows
/// that say almost the same thing. So the two halves are folded into one
/// [ToolCall]: [arguments] from the first, [result] from the second, and
/// `result == null` meaning "still running".
///
/// Replay is different in shape and the same in outcome: `episode_tool_calls`
/// stores **one** row per invocation (written when the call returns), so
/// [ToolCall.replayed] builds an already-paired entry directly.
///
/// Kept as a view-model rather than reusing `ChatToolEvent` directly so the
/// conversation can hold a stable list of them after the stream has closed.
class ToolCall {
  const ToolCall({
    required this.name,
    this.path,
    this.arguments,
    this.result,
    this.failed = false,
    this.diff,
  });

  /// A call read back from `GET /sessions/{id}`. Already terminal — the row is
  /// only written once the call has returned, so there is no pending state and
  /// no separate argument string (the daemon's stored summary is the result).
  factory ToolCall.replayed(Map<String, dynamic> json) {
    final name = asString(json['name'], 'tool');
    return ToolCall(
      name: name,
      path: asStringOrNull(json['path']),
      result: _stripPrefix(asStringOrNull(json['summary']), '$name ') ?? '已完成',
      failed: json['ok'] == false,
      diff: asStringOrNull(json['diff']),
    );
  }

  /// 这次写入改了什么（统一 diff，已截断）。null = 没有可看的改动。
  ///
  /// 只有 `write_file` 会有：只有它手上同时握着旧内容与新内容。shell 跑完
  /// 之后文件变成什么样，agent 并不知道。
  final String? diff;

  /// e.g. `memory_search`.
  final String name;

  /// The file this call operated on, straight from the wire.
  ///
  /// `ChatEvent::Tool` and `ToolCallDto` both carry `path: Option<String>`, and
  /// the daemon fills it from the tool's own `path` argument rather than from
  /// anything rendered. Null for tools that do not touch the filesystem —
  /// `memory_search` has no path, and inventing an empty string for it would
  /// draw a file row pointing at the workspace root.
  ///
  /// This used to be recovered by regex from [arguments]. That worked only for
  /// as long as `compact_args` kept rendering `(k=v, k=v)` with sorted keys: a
  /// reworded summary would not fail, it would point at **a different file**.
  final String? path;

  /// Compacted call arguments, e.g. `(query=pgvector 索引)`. The daemon already
  /// truncates long values — a `write_file` payload must not be able to blow up
  /// the SSE frame. Shown only when there is no [path] to show instead.
  final String? arguments;

  /// shell 调用的**命令本身**，从 [arguments] 的 `(command=…)` 包装里剥出来。
  ///
  /// 终端页签把它接在 `$ ` 提示符后面 —— 不剥的话画出来是
  /// `$ (command=git status)`，伪终端的形式感反而放大了内容的不对。
  /// 剥不出来（不是 shell、或 daemon 换了包装格式）就原样返回，
  /// 宁可少一层格式也不能把参数弄丢。
  String? get shellCommand {
    final a = arguments;
    if (a == null) return null;
    const prefix = '(command=';
    if (a.startsWith(prefix) && a.endsWith(')')) {
      return a.substring(prefix.length, a.length - 1);
    }
    return a;
  }

  /// One-line outcome, e.g. `返回 12 行 / 340 字符`. Null while the call is in
  /// flight.
  final String? result;

  /// The daemon reported a failure. Surfaced, not hidden: a denied path or a
  /// missing file explains an otherwise puzzling answer.
  ///
  /// Live turns infer it from the summary (the SSE event has no `ok` field);
  /// replayed ones read `episode_tool_calls.ok`, which is authoritative.
  final bool failed;

  bool get pending => result == null;

  /// A tool that reaches the filesystem, i.e. one that only works when the
  /// session is bound to a workspace.
  ///
  /// Still keyed on the name rather than on `path != null`: the icon should say
  /// "this touched your disk" even for a `list_dir` whose path the daemon
  /// clamped away, and a future non-file tool that happens to take a `path`
  /// argument should not be mislabelled.
  bool get touchesFiles =>
      name == 'read_file' || name == 'write_file' || name == 'list_dir';

  ToolCall copyWith({
    String? path,
    String? arguments,
    String? result,
    bool? failed,
    String? diff,
  }) => ToolCall(
    name: name,
    path: path ?? this.path,
    arguments: arguments ?? this.arguments,
    result: result ?? this.result,
    failed: failed ?? this.failed,
    diff: diff ?? this.diff,
  );

  /// Folds one `tool` event into [calls], returning a new list.
  ///
  /// 配对靠事件自己带的 [ToolPhase]：`call` 一律**开新行**，`result` 合进
  /// 最后一条同名且还没结果的行。
  ///
  /// # ⚠️ 从前是猜的，而那个猜法有个洞
  ///
  /// 旧规则是「同名且上一行还 pending 就当结果」。agent 循环一次派一个、
  /// 等它回来再派下一个，所以那条启发式**在今天成立** —— 但它成立得很脆：
  /// 一旦有两次同名调用在同一时刻派出去（并行工具调用），第二条 `call`
  /// 会被当成第一条的**结果**吃掉，于是两次调用只画出一行，而第二张图
  /// 从头到尾不出现。不报错、不崩，只是少了一样东西。
  ///
  /// 服务端 2026-08-23 起明说这一位（`ChatEvent::Tool.phase`），
  /// 就不必再猜。老服务端不发时缺省是 `result`，退化成旧行为。
  static List<ToolCall> merge(
    List<ToolCall> calls,
    String name,
    String? summary, {
    String? path,
    String? diff,
    ToolPhase phase = ToolPhase.result,
  }) {
    if (phase == ToolPhase.result &&
        calls.isNotEmpty &&
        calls.last.name == name &&
        calls.last.pending) {
      final result = _stripPrefix(summary, '$name ');
      return [
        ...calls.take(calls.length - 1),
        calls.last.copyWith(
          // The daemon repeats `path` on the result event, but a build that
          // only sent it on dispatch must not make the path disappear halfway
          // through the row's life.
          path: path,
          // diff 只随**结果**那条事件到达（调用那一刻还没执行），
          // 所以在这里合进去，而不是开行的时候
          diff: diff,
          result: result ?? '已完成',
          failed: result != null && result.startsWith('失败'),
        ),
      ];
    }
    return [
      ...calls,
      ToolCall(
        name: name,
        path: path,
        arguments: _stripPrefix(summary, '调用 $name '),
      ),
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
