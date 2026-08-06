import '../models/chat_event.dart';
import '../models/chat_session.dart';
import '../models/episode.dart';
import '../models/health_status.dart';
import '../models/memory_search_result.dart';

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

  void dispose();
}
