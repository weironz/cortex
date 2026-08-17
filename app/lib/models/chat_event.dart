import 'json.dart';
import 'pending_confirmation.dart';

/// One decoded frame of the `POST /chat` SSE stream.
///
/// Wire contract (each SSE `data:` payload is one JSON object):
/// * `{"type":"tool","name":"read_file","summary":"...","path":"src/main.rs"}`
/// * `{"type":"delta","text":"..."}`
/// * `{"type":"confirm","token":"...","tool":"shell","risk":"execute",
///    "preview":"command: rm -rf …","timeout_secs":180}`
/// * `{"type":"queued","ahead":1}`
/// * `{"type":"done","episode_id":"01J..."}`
/// * `{"type":"error","message":"..."}`
///
/// Observed order: `tool` → N×`delta` → `done`. Nothing in the client depends
/// on that order, though — `tool` may interleave with `delta` once the agent
/// loop does multi-step tool use.
///
/// Unknown `type` values decode to [ChatUnknownEvent] rather than throwing, so
/// a server that grows a new event type does not break older clients.
sealed class ChatEvent {
  const ChatEvent();

  factory ChatEvent.fromJson(Map<String, dynamic> json) {
    return switch (asString(json['type'])) {
      'queued' => ChatQueuedEvent(asIntOrNull(json['ahead']) ?? 1),
      'delta' => ChatDeltaEvent(asString(json['text'])),
      'tool' => ChatToolEvent(
        name: asString(json['name'], 'tool'),
        summary: asStringOrNull(json['summary']),
        path: asStringOrNull(json['path']),
        diff: asStringOrNull(json['diff']),
      ),
      'confirm' => ChatConfirmEvent(
        PendingConfirmation(
          token: asString(json['token']),
          tool: asString(json['tool'], 'tool'),
          risk: asString(json['risk'], 'execute'),
          scope: asStringOrNull(json['scope']),
          diff: asStringOrNull(json['diff']),
          preview: asString(json['preview']),
          // `timeout_secs` here, `expires_in_secs` on the recovery endpoint:
          // the same quantity named for the two different questions it answers
          // ("how long do they get" vs "how long is left"). Both are durations
          // from *now*, which is why neither path has to trust a clock.
          deadline: DateTime.now().add(
            Duration(
              seconds:
                  asIntOrNull(json['timeout_secs']) ??
                  PendingConfirmation.kDefaultTimeoutSecs,
            ),
          ),
        ),
      ),
      'done' => ChatDoneEvent(asStringOrNull(json['episode_id'])),
      'error' => ChatErrorEvent(
        asStringOrNull(json['message']) ?? '服务端返回了一个未描述的错误',
      ),
      final other => ChatUnknownEvent(other, json),
    };
  }
}

/// 这一轮排在队里，前面还有 [ahead] 轮没跑完。
///
/// **不是终态。** 同一条流接着会送这一轮自己的 delta / tool，最后一条 `done`。
///
/// 为什么服务端要发它：一个会话一次只跑一轮，排队期间这条流上除了 keepalive
/// 什么都没有 —— 不说一声的话界面就是一个转了几分钟的圈，与卡死一模一样。
final class ChatQueuedEvent extends ChatEvent {
  const ChatQueuedEvent(this.ahead);

  /// 前面还有几轮。服务端保证 `>= 1`；缺字段时按 1 处理（说「在排队」总比
  /// 说「排在第 0 位」好）。
  final int ahead;
}

/// Incremental assistant text. Append, never replace.
final class ChatDeltaEvent extends ChatEvent {
  const ChatDeltaEvent(this.text);
  final String text;
}

/// The agent invoked a tool. Rendered as a collapsible one-liner in the
/// conversation, not as message content.
final class ChatToolEvent extends ChatEvent {
  const ChatToolEvent({required this.name, this.summary, this.path, this.diff});
  final String name;
  final String? summary;

  /// The file this call touched, or null for a tool that touches none.
  /// Optional in the contract precisely so that a tool touching no file can
  /// omit it rather than send an empty string the UI would render as a path.
  final String? path;

  /// 这次写入改了什么。只随**结果**那条事件到达 —— 调用那一刻还没执行，
  /// 也就还没有改动可言。
  final String? diff;
}

/// The turn is **suspended** until this is answered.
///
/// Not a terminal frame and not an error: the stream stays open, deltas simply
/// stop arriving until a receipt reaches the daemon (or its timeout fires, and
/// silence counts as a refusal). The UI must therefore keep the streaming
/// bubble alive while showing the prompt — treating this like an error would
/// discard text the model already produced.
final class ChatConfirmEvent extends ChatEvent {
  const ChatConfirmEvent(this.request);
  final PendingConfirmation request;
}

/// Terminal frame. [episodeId] is the archived assistant episode.
final class ChatDoneEvent extends ChatEvent {
  const ChatDoneEvent(this.episodeId);
  final String? episodeId;
}

/// Terminal frame carrying a server-side failure.
final class ChatErrorEvent extends ChatEvent {
  const ChatErrorEvent(this.message);
  final String message;
}

/// Forward-compatibility escape hatch.
final class ChatUnknownEvent extends ChatEvent {
  const ChatUnknownEvent(this.type, this.raw);
  final String type;
  final Map<String, dynamic> raw;
}
