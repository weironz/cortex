import 'json.dart';

/// A file the daemon has agreed to parse, in whichever way it got there.
///
/// Exists so the 97 MB upload happens **once**. Preview and run are two
/// requests, and without a handle in between the browser would send the whole
/// file twice — the second time while the user is watching a progress bar that
/// has not started yet.
sealed class ImportTarget {
  const ImportTarget();

  /// The `{path}` / `{handle}` half of an import request body.
  Map<String, Object?> get locator;
}

/// Desktop: the local agent opens this path itself.
final class ImportTargetPath extends ImportTarget {
  const ImportTargetPath(this.path);

  final String path;

  @override
  Map<String, Object?> get locator => {'path': path};
}

/// Web: `POST /import/upload` already put the bytes in a spool file.
final class ImportTargetHandle extends ImportTarget {
  const ImportTargetHandle(this.handle, {required this.expiresInSecs});

  final String handle;

  /// How long the spool file lives. Surfaced so the UI can warn *before* the
  /// user comes back an hour later to a dead handle and a 97 MB re-upload.
  final int expiresInSecs;

  @override
  Map<String, Object?> get locator => {'handle': handle};
}

/// The bill, computed server-side and shown before anything is written.
///
/// Every number here is the **daemon's**, not the client's. The rule is the
/// same one `cortex_import::Estimate` states: the figure on screen and the
/// figure the CLI prints must come from one calculation, or they will drift and
/// nobody will notice until the two disagree in front of a user.
class ImportEstimate {
  const ImportEstimate({
    required this.platform,
    required this.conversations,
    required this.messages,
    required this.pairs,
    required this.tokens,
    required this.unpaired,
    required this.minutes,
    this.earliest,
    this.latest,
  });

  /// "ChatGPT" / "Claude", already written for a human.
  final String platform;
  final int conversations;
  final int messages;

  /// Extraction calls. **This number is the money** — one LLM call per pair.
  final int pairs;
  final int tokens;

  /// Messages that will land as raw text but produce no facts (a conversation
  /// that opens with the assistant, or a last question nobody answered).
  ///
  /// Shown separately because otherwise "imported 100 messages" followed by
  /// "40 new memories" looks like a bug.
  final int unpaired;

  /// Lower bound in minutes, from the client-side pacing the daemon applies.
  final double minutes;

  final DateTime? earliest;
  final DateTime? latest;

  static ImportEstimate fromJson(Map<String, Object?> json) => ImportEstimate(
    platform: asStringOrNull(json['platform']) ?? '未知来源',
    conversations: asInt(json['conversations']),
    messages: asInt(json['messages']),
    pairs: asInt(json['pairs']),
    tokens: asInt(json['tokens']),
    unpaired: asInt(json['unpaired']),
    minutes: asDoubleOrNull(json['minutes']) ?? 0,
    earliest: asDateOrNull(json['earliest']),
    latest: asDateOrNull(json['latest']),
  );
}

/// One frame of `POST …/import/run`.
sealed class ImportEvent {
  const ImportEvent();
}

/// The bill again, recomputed at the moment work actually starts.
///
/// The client already has one from preview, but that may be minutes old and the
/// filters may have changed since. This one is authoritative.
class ImportStartedEvent extends ImportEvent {
  const ImportStartedEvent(this.estimate);
  final ImportEstimate estimate;
}

class ImportProgressEvent extends ImportEvent {
  const ImportProgressEvent({
    required this.conversationsDone,
    required this.conversationsTotal,
    required this.pairsDone,
    required this.skipped,
    required this.failures,
  });

  final int conversationsDone;
  final int conversationsTotal;
  final int pairsDone;

  /// Recognised as "already imported" by the daemon — **not billed again**.
  final int skipped;
  final int failures;
}

class ImportDoneEvent extends ImportEvent {
  const ImportDoneEvent({
    required this.pairsDone,
    required this.skipped,
    required this.failures,
  });

  final int pairsDone;
  final int skipped;

  /// Non-zero means "run it again" — writes are idempotent, so a rerun only
  /// fills the gaps and costs nothing for what already landed.
  final int failures;
}

class ImportErrorEvent extends ImportEvent {
  const ImportErrorEvent(this.message);
  final String message;
}
