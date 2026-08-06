/// A tool invocation surfaced by the agent loop during a turn.
///
/// Carried by the `tool` SSE event. Kept as a view-model rather than reusing
/// [ChatToolEvent] directly so the conversation can hold a stable list of them
/// after the stream has closed.
class ToolCall {
  const ToolCall({required this.name, this.summary});

  /// e.g. `memory_search`.
  final String name;

  /// Server-rendered one-line description of what the call did.
  final String? summary;
}
