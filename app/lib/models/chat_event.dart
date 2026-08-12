import 'json.dart';
import 'memory_fact.dart';
import 'pending_confirmation.dart';

/// One decoded frame of the `POST /chat` SSE stream.
///
/// Wire contract (each SSE `data:` payload is one JSON object):
/// * `{"type":"memory","facts":[{...}]}`
/// * `{"type":"tool","name":"read_file","summary":"...","path":"src/main.rs"}`
/// * `{"type":"delta","text":"..."}`
/// * `{"type":"confirm","token":"...","tool":"shell","risk":"execute",
///    "preview":"command: rm -rf …","timeout_secs":180}`
/// * `{"type":"done","episode_id":"01J..."}`
/// * `{"type":"error","message":"..."}`
///
/// Observed order from `cortexd`: `memory` → `tool` → N×`delta` → `done`.
/// Nothing in the client depends on that order, though — `memory` and `tool`
/// may interleave with `delta` once the agent loop does multi-step tool use.
///
/// Unknown `type` values decode to [ChatUnknownEvent] rather than throwing, so
/// a server that grows a new event type does not break older clients.
sealed class ChatEvent {
  const ChatEvent();

  factory ChatEvent.fromJson(Map<String, dynamic> json) {
    return switch (asString(json['type'])) {
      'delta' => ChatDeltaEvent(asString(json['text'])),
      'memory' => ChatMemoryEvent(
        asObjectList(json['facts']).map(MemoryFact.fromJson).toList(),
      ),
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

/// Incremental assistant text. Append, never replace.
final class ChatDeltaEvent extends ChatEvent {
  const ChatDeltaEvent(this.text);
  final String text;
}

/// The memory facts injected into this turn's prompt.
final class ChatMemoryEvent extends ChatEvent {
  const ChatMemoryEvent(this.facts);
  final List<MemoryFact> facts;
}

/// The agent invoked a tool. Rendered as a collapsible one-liner in the
/// conversation, not as message content.
final class ChatToolEvent extends ChatEvent {
  const ChatToolEvent({required this.name, this.summary, this.path, this.diff});
  final String name;
  final String? summary;

  /// The file this call touched, or null for a tool that touches none.
  /// Optional in the contract precisely so that `memory_search` can omit it
  /// rather than send an empty string the UI would render as a path.
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
