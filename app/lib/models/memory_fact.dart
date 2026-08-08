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
    this.sourceChannel,
    this.trustTier,
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

  /// Which channel this fact came in through: `user_stated`, `conversation`,
  /// `derived`, `tool_output`, `external`, `unknown_legacy`.
  ///
  /// Not to be confused with [RetrievalChannels] — those are the *recall*
  /// paths that found this fact (`bm25`, `vector`, …). Two different things
  /// that the wire protocol unhelpfully both calls a channel. This one is
  /// about where the knowledge came from; that one is about how we found it
  /// again.
  ///
  /// Null on daemons older than the field, and on rows that predate the
  /// column (`unknown_legacy` reaches us as a value, absence as null).
  final String? sourceChannel;

  /// Trust level, 1 being highest. Paired with [sourceChannel] by a database
  /// CHECK constraint, and sent separately rather than derived client-side —
  /// re-deriving it here would be a third copy of a mapping that already
  /// exists in Rust and in SQL, and the third copy has nothing keeping it
  /// honest.
  final int? trustTier;

  factory MemoryFact.fromJson(Map<String, dynamic> json) => MemoryFact(
    id: asString(json['id']),
    statement: asString(json['statement']),
    predicate: asStringOrNull(json['predicate']),
    domain: asStringOrNull(json['domain']),
    confidence: asDoubleOrNull(json['confidence']),
    validAt: asDateOrNull(json['valid_at']),
    createdAt: asDateOrNull(json['created_at']),
    sourceEpisodeId: asStringOrNull(json['source_episode_id']),
    sourceChannel: asStringOrNull(json['source_channel']),
    trustTier: asIntOrNull(json['trust_tier']),
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
    if (sourceChannel != null) 'source_channel': sourceChannel,
    if (trustTier != null) 'trust_tier': trustTier,
  };

  @override
  bool operator ==(Object other) => other is MemoryFact && other.id == id;

  @override
  int get hashCode => id.hashCode;
}
