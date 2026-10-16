part of '../http_cortex_api.dart';

mixin _SyncEndpoints on HttpCortexApiBase {
  @override
  Future<SyncPage> sync({required int since, int limit = 500}) async {
    final json = await _getJson('/sync', {
      'since': '$since',
      'limit': '$limit',
    });
    return SyncPage.fromJson(json);
  }

  @override
  Stream<SyncEvent> watchSync() async* {
    // Minted *before* the handshake rather than lazily on 401: a WebSocket
    // rejected at the HTTP upgrade gives back a bare "connection failed" with
    // no status code to inspect, so there would be nothing to retry *on*.
    final String? ticket;
    try {
      ticket = await _currentTicket();
    } on CortexApiException catch (e) {
      // Includes the 401 that says the long-lived token itself is bad, which
      // `_failure` has already reported upward. Surfaced as a link failure so
      // `SyncController` backs off instead of hot-looping while the login gate
      // takes over.
      throw CortexApiException('换取实时同步票据失败：${e.message}', cause: e);
    }

    final WebSocketChannel channel;
    try {
      channel = WebSocketChannel.connect(_wsUri(ticket));
      // `connect` is lazy — without awaiting `ready` a failed handshake would
      // only surface later, as an error on the message stream, and the caller
      // could not tell "never connected" from "dropped after 3 hours".
      await channel.ready;
    } on Object catch (e) {
      throw CortexApiException(_wsUnreachableMessage(e), cause: e);
    }

    // 为什么不是一句 `await for (final frame in channel.stream)`
    //
    // 那样写，**取消这条订阅会挂死**。`await for` 在被取消时要先取消它对
    // `channel.stream` 的内层订阅，而 WebSocket 的取消要走完关闭握手 ——
    // 等服务端回一帧 close。可读侧此刻正在被撤掉，那一帧永远读不到：
    // `cancel()` 等内层，内层等一帧永远不来的数据。
    //
    // 挂住的不是别的路径，正是**切后端与退登录**：两处都要先断开实时同步
    // 再往下走。症状是点一下没反应，而且没有任何报错。
    //
    // 改成自己持有内层订阅、由 `onCancel` 决定怎么收尾：两件清理都发出去
    // 但都不等 —— 关闭是纯清理，没有任何人需要它完成的那一刻。
    final events = StreamController<SyncEvent>();
    final frames = channel.stream.listen(
      (frame) {
        if (frame is! String) return; // the daemon only sends text frames
        final Object? decoded;
        try {
          decoded = jsonDecode(frame);
        } on FormatException {
          // One malformed frame must not tear down a healthy link; the next
          // bump will carry us forward anyway.
          return;
        }
        if (decoded is Map<String, dynamic>) {
          events.add(SyncEvent.fromJson(decoded));
        }
      },
      // An abnormal close arrives as an error. Normalise it so the caller only
      // ever has to handle one error type.
      onError: (Object e) =>
          events.addError(CortexApiException('实时同步通道断开：$e', cause: e)),
      onDone: events.close,
      cancelOnError: true,
    );
    events.onCancel = () {
      unawaited(frames.cancel());
      unawaited(channel.sink.close());
    };

    yield* events.stream;
  }

  // ------------------------------------------------------------------- blobs
}
