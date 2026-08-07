import 'json.dart';
import 'memory_fact.dart';

/// One memory fact that was injected into a turn's prompt.
///
/// ## Why this is not just a [MemoryFact]
///
/// The two paths that produce it carry different things, and flattening them
/// into one type would mean pretending each has what the other has:
///
/// * **Live** (`memory` SSE event) ships a full `FactDto` — predicate,
///   confidence, both timestamps — but says nothing about *why* it was
///   retrieved or whether it still holds.
/// * **Replay** (`GET /sessions/{id}` → `episode.memories`) ships
///   `InjectedMemoryDto`: the retrieval [channels] and [score] that put it in
///   the prompt, plus [invalidated] — and only `statement` / `domain` of the
///   fact itself.
///
/// So this wraps an optional [fact] and adds the attribution around it. What is
/// unknown stays null rather than being defaulted to something plausible.
class InjectedMemory {
  const InjectedMemory({
    required this.factId,
    this.fact,
    this.channels = const [],
    this.score,
    this.invalidated = false,
  });

  /// Straight from a live `memory` event: everything about the fact is known,
  /// nothing about its retrieval is.
  InjectedMemory.live(MemoryFact this.fact)
    : factId = fact.id,
      channels = const [],
      score = null,
      invalidated = false;

  final String factId;

  /// Null means the fact's row is **gone** — redacted or purged since the turn
  /// ran. The server sends `statement: null` for exactly this case and the UI
  /// must render a placeholder: hiding the entry would be editing the replay,
  /// which is the one thing an audit trail may not do.
  final MemoryFact? fact;

  /// Which retrieval channels matched (`bm25` / `vector` / `graph` / …).
  /// Empty on the live path — the SSE event does not carry attribution.
  final List<String> channels;

  /// Fused RRF score, when the server recorded one.
  final double? score;

  /// The fact has been superseded **since** this turn ran.
  ///
  /// Deliberately still shown. "The thing the answer leaned on is no longer
  /// true" is the single most useful line an audit trail can produce, and
  /// filtering it out would silently make old answers look better-founded than
  /// they were.
  final bool invalidated;

  /// True when the fact row no longer exists — see [fact].
  bool get redacted => fact == null;

  factory InjectedMemory.fromJson(Map<String, dynamic> json) {
    final factId = asString(json['fact_id']);
    final statement = asStringOrNull(json['statement']);
    return InjectedMemory(
      factId: factId,
      fact: statement == null
          ? null
          : MemoryFact(
              id: factId,
              statement: statement,
              domain: asStringOrNull(json['domain']),
              sourceEpisodeId: asStringOrNull(json['source_episode_id']),
            ),
      channels: asStringList(json['channels']),
      score: asDoubleOrNull(json['score']),
      invalidated: json['invalidated'] == true,
    );
  }
}
