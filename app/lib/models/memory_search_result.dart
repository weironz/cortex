import 'json.dart';
import 'memory_fact.dart';

/// Which retrieval channels surfaced a given fact, and its fused score.
///
/// `cortexd` runs BM25 and vector search in parallel and fuses with RRF; the
/// per-fact channel list is what makes "why did this come back?" answerable in
/// the UI instead of being a black box.
class RetrievalChannels {
  const RetrievalChannels({
    required this.factId,
    required this.channels,
    this.score,
  });

  final String factId;

  /// e.g. `['bm25', 'vector']`. Both present means the two channels agreed.
  final List<String> channels;

  /// Fused relevance score.
  final double? score;

  factory RetrievalChannels.fromJson(Map<String, dynamic> json) =>
      RetrievalChannels(
        factId: asString(json['fact_id']),
        channels: asStringList(json['channels']),
        score: asDoubleOrNull(json['score']),
      );
}

/// `GET /memory/search` response.
class MemorySearchResult {
  const MemorySearchResult({required this.facts, this.channels = const {}});

  final List<MemoryFact> facts;

  /// Indexed by fact id for O(1) lookup while building the result list.
  final Map<String, RetrievalChannels> channels;

  static const empty = MemorySearchResult(facts: []);

  bool get isEmpty => facts.isEmpty;

  factory MemorySearchResult.fromJson(Map<String, dynamic> json) {
    final channels = <String, RetrievalChannels>{};
    for (final raw in asObjectList(json['channels'])) {
      final c = RetrievalChannels.fromJson(raw);
      if (c.factId.isNotEmpty) channels[c.factId] = c;
    }
    return MemorySearchResult(
      facts: asObjectList(
        json['facts'],
      ).map(MemoryFact.fromJson).toList(growable: false),
      channels: channels,
    );
  }
}
