import 'chat_session.dart';
import 'episode.dart';
import 'json.dart';

/// `GET /sessions/{id}` — the session overview plus **one page** of episodes.
///
/// ## The page is the newest one, and it pages backwards
///
/// `?limit=N` picks the page size (server default and hard cap are both 500),
/// `?before=<cursor>` walks towards older messages. With neither, the response
/// is the *latest* N turns — which is what a user opening a conversation wants
/// to see, and the opposite of what the uncursored `ORDER BY occurred_at ASC
/// LIMIT 500` used to return.
///
/// [episodes] is chronological (oldest first) *within* the page: the server
/// reverses after slicing so a client can render the array in order. Paging up
/// therefore means **prepending** the next response to what is already held.
///
/// [hasMore] is authoritative and must not be re-derived from
/// `episodes.length == limit` — the server over-fetches by one row precisely so
/// that an exactly-divisible history does not produce a phantom empty page.
class SessionDetail {
  const SessionDetail({
    required this.session,
    required this.episodes,
    this.hasMore = false,
    this.nextCursor,
  });

  final ChatSession session;

  /// This page, oldest first.
  final List<Episode> episodes;

  /// There are older messages before this page.
  final bool hasMore;

  /// Pass as `before` to fetch the page before this one. Null when [hasMore] is
  /// false. Its format is **opaque** to the client — the server validates it and
  /// answers 400 on a malformed one, so there is nothing to gain from parsing.
  final String? nextCursor;

  factory SessionDetail.fromJson(Map<String, dynamic> json) => SessionDetail(
    // The session fields are flattened into the same object.
    session: ChatSession.fromJson(json),
    episodes: asObjectList(
      json['episodes'],
    ).map(Episode.fromJson).toList(growable: false),
    hasMore: json['has_more'] == true,
    nextCursor: asStringOrNull(json['next_cursor']),
  );
}
