import 'json.dart';

/// A high-risk tool call that is **already suspended server-side**, waiting for
/// this user to allow or deny it.
///
/// Arrives on two paths that carry the same information under different names:
///
/// | source | shape | session id | remaining time |
/// |---|---|---|---|
/// | SSE `{"type":"confirm"}` | `ChatEvent::Confirm` | *absent* | `timeout_secs` |
/// | `GET /confirmations` | `PendingInfo` | present | `expires_in_secs` |
///
/// The SSE event omits `session_id` because the stream it arrives on already
/// belongs to one; the client fills it in from context. Everything else is
/// identical by design — the daemon states that the event must be
/// self-sufficient, so a client never has to correlate it with the `tool` event
/// that may or may not have preceded it.
class PendingConfirmation {
  const PendingConfirmation({
    required this.token,
    required this.tool,
    required this.risk,
    required this.preview,
    required this.deadline,
    this.sessionId,
    this.scope,
    this.diff,
  });

  /// One-shot credential, echoed back verbatim in the receipt.
  final String token;

  /// The tool the agent wants to run, e.g. `shell`.
  final String tool;

  /// `"execute"` / `"write"`.
  final String risk;

  /// The full arguments, rendered for a human by the daemon.
  ///
  /// **Never truncate this again.** `cortexd::confirm::preview_of` already
  /// capped it at 8 KiB and, when it did, appended a visible marker saying so.
  /// A second trim here would cut exactly the part the server went out of its
  /// way to preserve — the tail of a command is where `| sh` lives.
  final String preview;

  /// When this stops being answerable, in **local monotonic-ish terms**:
  /// computed as `now + remaining` at the moment of receipt.
  ///
  /// Deliberately not derived from the server's `asked_at` timestamp. That
  /// would make the countdown depend on the two clocks agreeing, and a client
  /// whose clock is three minutes fast would show every confirmation as already
  /// expired. The server sends a *duration* precisely so nobody has to compare
  /// clocks; the only error left is network latency, which is bounded by the
  /// round trip and always in the safe direction (we expire slightly early).
  final DateTime deadline;

  /// Which conversation this belongs to. Null only in the instant between
  /// decoding an SSE event and the controller stamping it.
  final String? sessionId;

  /// 这次要碰的**工作区外**的绝对路径。null = 在工作区内。
  ///
  /// # 这个字段在协议里躺了整整一轮没人读
  ///
  /// 服务端从加上它那天起就一直在发（`ChatEvent::Confirm.scope`，那边的
  /// 文档甚至写明了「界面要据此把话说清楚」），而客户端的模型里根本没有
  /// 它 —— 于是「批准一次越界写入」与「批准一次普通写入」在屏幕上长得
  /// 一模一样，用户看到的只是一个 `path` 参数，判断不出那是工作区里的
  /// 还是桌面上的。
  ///
  /// 这是本仓库第 8 次「造好了但没人调用」。
  final String? scope;

  /// 这次写入会把文件改成什么样。null = 没有可看的改动。
  ///
  /// 与 [scope] 治的是同一件事的两个轴：scope 回答「写到哪」，
  /// diff 回答「写什么」。缺任何一个，「允许」这个按钮都是在盲签。
  final String? diff;

  Duration remainingFrom(DateTime now) {
    final left = deadline.difference(now);
    return left.isNegative ? Duration.zero : left;
  }

  bool isExpiredAt(DateTime now) => !deadline.isAfter(now);

  PendingConfirmation withSession(String id) => PendingConfirmation(
    token: token,
    tool: tool,
    risk: risk,
    preview: preview,
    deadline: deadline,
    sessionId: id,
    scope: scope,
    diff: diff,
  );

  /// Decodes one entry of `GET /confirmations`.
  ///
  /// [now] is injected so the deadline is anchored to the same instant for
  /// every entry of a page, and so tests can pin it.
  factory PendingConfirmation.fromJson(
    Map<String, dynamic> json, {
    required DateTime now,
  }) => PendingConfirmation(
    token: asString(json['token']),
    tool: asString(json['tool'], 'tool'),
    risk: asString(json['risk'], 'execute'),
    preview: asString(json['preview']),
    // Absent or unparseable means "we do not know how long is left". Zero would
    // render as already-expired and hide a live request; the daemon's own
    // default is the least-wrong stand-in.
    deadline: now.add(
      Duration(seconds: asIntOrNull(json['expires_in_secs']) ?? kDefaultTimeoutSecs),
    ),
    sessionId: asStringOrNull(json['session_id']),
    scope: asStringOrNull(json['scope']),
    diff: asStringOrNull(json['diff']),
  );

  /// `cortexd::confirm::DEFAULT_TIMEOUT_SECS`. Only ever a fallback — every
  /// real message carries its own value, because the deployment can override it.
  static const int kDefaultTimeoutSecs = 180;
}
