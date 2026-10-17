part of '../mock_cortex_api.dart';

mixin _SyncMock on MockCortexApiBase {
  /// The fixtures have no writer, so the link is permanently "connected and
  /// caught up": one `hello`, then silence.
  ///
  /// `SyncController` short-circuits on the mock source and never calls this,
  /// but the contract stays implemented — a stub that threw would make the
  /// mock a *different* API, which is precisely what this class exists to
  /// avoid.
  @override
  Stream<SyncEvent> watchSync() {
    final controller = StreamController<SyncEvent>();
    controller.onListen = () {
      if (_disposed) {
        controller.addError(const CortexApiException('Mock 数据源已关闭'));
        return;
      }
      controller.add(const SyncHello(cursor: 0, version: '0.0.1-mock'));
    };
    return controller.stream;
  }

  @override
  Future<SyncPage> sync({required int since, int limit = 500}) async {
    await _latency(60);
    // Already caught up by construction: echo the caller's cursor back so the
    // convergence check ("pull until empty") terminates the same way it does
    // against the real daemon.
    return SyncPage(cursor: since, hasMore: false);
  }

  // -------------------------------------------------------------------- chat

  // ------------------------------------------------------------------- blobs
}
