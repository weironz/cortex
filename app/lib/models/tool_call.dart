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
    this.output,
    this.subagent,
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

  /// 这次调用的**真实输出**，服务端已截到 2 KB（头尾各留，中间注明省略了
  /// 多少）。null = 没有正文可看。
  ///
  /// 界面据它决定给不给展开箭头 —— 一个点下去空空如也的箭头比没有箭头更让人
  /// 困惑（`_ToolRow` 里对 diff 与报错是同一套判断）。
  ///
  /// ⚠️ **回放拿不到它**：`episode_tool_calls` 里没有这一列，所以打开一个旧
  /// 会话时工具行只有摘要。这是有意的取舍，不是漏了 —— 存它要一次 schema
  /// 迁移，而按一轮 20 次调用算是每轮 40 KB 的历史数据。
  final String? output;

  /// 这次写入改了什么（统一 diff，已截断）。null = 没有可看的改动。
  ///
  /// 只有 `write_file` 会有：只有它手上同时握着旧内容与新内容。shell 跑完
  /// 之后文件变成什么样，agent 并不知道。
  final String? diff;

  /// 这一行是**哪个子 agent** 干的。`null` = 主 agent 自己。
  ///
  /// 界面据它分组缩进；[merge] 也据它配对 —— 见那里的说明，
  /// 四路并行时不看这个字段会把 A 的结果接到 B 的调用上。
  final SubagentTag? subagent;

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
    String? output,
  }) => ToolCall(
    name: name,
    path: path ?? this.path,
    arguments: arguments ?? this.arguments,
    result: result ?? this.result,
    failed: failed ?? this.failed,
    diff: diff ?? this.diff,
    output: output ?? this.output,
    // copyWith 不改它 —— 一行工具属于谁，从它被创建那一刻起就定了
    subagent: subagent,
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
    String? output,
    ToolPhase phase = ToolPhase.result,
    SubagentTag? subagent,
  }) {
    if (phase == ToolPhase.result) {
      // ⚠️ **倒着找，而且要比对 subagent —— 不能只看 `calls.last`。**
      //
      // 子 agent 是**四路并行**的：A 开工、B 开工、A 收工、B 收工 是常态。
      // 只看最后一条的话，A 的结果会接到 B 那条待完成的调用上 —— 而那
      // 不报错，只是把一个工具的结果显示在另一个工具名下。
      //
      // 主 agent 自己那些行照旧（`subagent` 两边都是 null 时判等成立），
      // 而它本来就是串行的，倒着找第一条就是原来的 `calls.last`。
      for (var i = calls.length - 1; i >= 0; i--) {
        final c = calls[i];
        if (c.name != name || !c.pending || !_sameAgent(c.subagent, subagent)) {
          continue;
        }
        final result = _stripPrefix(summary, '$name ');
        return [
          ...calls.take(i),
          c.copyWith(
            // The daemon repeats `path` on the result event, but a build that
            // only sent it on dispatch must not make the path disappear halfway
            // through the row's life.
            path: path,
            // diff 与 output 都只随**结果**那条事件到达（调用那一刻还没
            // 执行完），所以在这里合进去，而不是开行的时候
            diff: diff,
            output: output,
            result: result ?? '已完成',
            failed: result != null && result.startsWith('失败'),
          ),
          ...calls.skip(i + 1),
        ];
      }
    }
    return [
      ...calls,
      ToolCall(
        name: name,
        path: path,
        arguments: _stripPrefix(summary, '调用 $name '),
        subagent: subagent,
      ),
    ];
  }

  /// 两条事件是不是同一个 agent 发的。
  ///
  /// 只比 `index`：`task` 是同一次派发里带的同一串，比它只是多一次字符串
  /// 比较；而万一主 agent 改了措辞重发，按 task 比会配不上对，表现是
  /// 那一行永远停在「进行中」。
  static bool _sameAgent(SubagentTag? a, SubagentTag? b) =>
      a?.index == b?.index;

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
