import '../models/chat_event.dart';
import '../models/chat_session.dart';
import '../models/episode.dart';
import '../models/health_status.dart';
import '../models/memory_search_result.dart';
import '../models/sync_event.dart';
import '../models/sync_record.dart';

/// The whole surface the UI is allowed to touch.
///
/// Two implementations exist — [HttpCortexApi] (real `cortexd`) and
/// [MockCortexApi] (in-memory fixtures). Which one is live is decided once, in
/// `cortexApiProvider`; no widget or controller knows the difference.
abstract interface class CortexApi {
  /// Human-readable name of the active backend, shown in the status strip.
  String get label;

  /// `GET /health`
  Future<HealthStatus> health();

  /// `POST /chat` → SSE.
  ///
  /// The returned stream is single-subscription and must be cancelled by the
  /// caller to abort an in-flight generation.
  Stream<ChatEvent> chat({required String sessionId, required String message});

  /// `GET /memory/search?q=&limit=&as_of=`
  ///
  /// [asOf] replays the memory as it was *known* at that instant (transaction
  /// time), not as it was *true* — this is the "what did I believe three
  /// months ago" axis of the bitemporal model. Null means "now".
  Future<MemorySearchResult> searchMemory(
    String query, {
    int limit = 20,
    DateTime? asOf,
  });

  /// `GET /episodes/{id}`
  Future<Episode> episode(String id);

  /// `GET /sessions`
  Future<List<ChatSession>> sessions();

  /// `GET /ws` — **one** connection attempt.
  ///
  /// The returned stream ends when the socket closes and errors when it cannot
  /// be opened. Reconnection is deliberately *not* handled here: backoff,
  /// attempt counting and the cursor are policy, and policy belongs to
  /// `SyncController` where it can be unit-tested against a fake socket.
  Stream<SyncEvent> watchSync();

  /// `GET /sync?since=&limit=`
  ///
  /// [since] must be the caller's **own** cursor. Passing a cursor taken from a
  /// [SyncEvent] would skip everything between what we have and what the server
  /// reached — see the contract note on [SyncEvent].
  Future<SyncPage> sync({required int since, int limit = 500});

  void dispose();
}
