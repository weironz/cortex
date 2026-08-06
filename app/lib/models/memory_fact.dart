import 'json.dart';

/// A single extracted memory fact.
///
/// Mirrors the `facts` rows returned by `GET /memory/search` and the entries
/// carried by the `memory` SSE event on `POST /chat`. Only [id] and
/// [statement] are guaranteed; the chat event ships a reduced projection, so
/// everything else is nullable.
class MemoryFact {
  const MemoryFact({
    required this.id,
    required this.statement,
    this.predicate,
    this.domain,
    this.confidence,
    this.validAt,
    this.createdAt,
    this.sourceEpisodeId,
  });

  /// ULID.
  final String id;

  /// Natural-language rendering of the fact — what we show to the user.
  final String statement;

  /// Relation name in the memory graph, e.g. `prefers`, `works_on`.
  final String? predicate;

  /// Coarse topical bucket, e.g. `coding`, `office`.
  final String? domain;

  /// Extractor confidence in `[0, 1]`.
  final double? confidence;

  /// Valid time — when the fact became true in the world.
  final DateTime? validAt;

  /// Transaction time — when Cortex learned it.
  final DateTime? createdAt;

  /// Provenance: the episode this fact was extracted from.
  final String? sourceEpisodeId;

  factory MemoryFact.fromJson(Map<String, dynamic> json) => MemoryFact(
    id: asString(json['id']),
    statement: asString(json['statement']),
    predicate: asStringOrNull(json['predicate']),
    domain: asStringOrNull(json['domain']),
    confidence: asDoubleOrNull(json['confidence']),
    validAt: asDateOrNull(json['valid_at']),
    createdAt: asDateOrNull(json['created_at']),
    sourceEpisodeId: asStringOrNull(json['source_episode_id']),
  );

  Map<String, dynamic> toJson() => {
    'id': id,
    'statement': statement,
    if (predicate != null) 'predicate': predicate,
    if (domain != null) 'domain': domain,
    if (confidence != null) 'confidence': confidence,
    if (validAt != null) 'valid_at': validAt!.toIso8601String(),
    if (createdAt != null) 'created_at': createdAt!.toIso8601String(),
    if (sourceEpisodeId != null) 'source_episode_id': sourceEpisodeId,
  };

  @override
  bool operator ==(Object other) => other is MemoryFact && other.id == id;

  @override
  int get hashCode => id.hashCode;
}
