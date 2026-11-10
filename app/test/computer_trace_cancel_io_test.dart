import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/computer_trace_api.dart';
import 'package:cortex_app/features/chat/widgets/computer_use_trace_viewer.dart';
import 'package:cortex_app/models/chat_event.dart';
import 'package:cortex_app/models/tool_call.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/chat_state.dart';
import 'package:cortex_app/state/confirm_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/io_client.dart';
import 'package:http/testing.dart';

class _RealLoopbackHttpOverrides extends HttpOverrides {}

HttpClient _loopbackHttpClient() => HttpOverrides.runWithHttpOverrides(() {
  return HttpClient()..findProxy = (_) => 'DIRECT';
}, _RealLoopbackHttpOverrides());

class _HeadersClient extends http.BaseClient {
  final http.Client _inner = IOClient(_loopbackHttpClient());
  final received409 = Completer<void>();

  @override
  Future<http.StreamedResponse> send(http.BaseRequest request) async {
    final response = await _inner.send(request);
    if (response.statusCode == 409 && !received409.isCompleted) {
      received409.complete();
    }
    return response;
  }

  @override
  void close() => _inner.close();
}

class _BlockedHeadersClient extends http.BaseClient {
  final http.Client _inner = IOClient(_loopbackHttpClient());
  final received409 = Completer<void>();
  final abortObserved = Completer<void>();
  final release = Completer<void>();

  @override
  Future<http.StreamedResponse> send(http.BaseRequest request) async {
    final response = await _inner.send(request);
    if (response.statusCode == 409 && !received409.isCompleted) {
      if (request is http.Abortable && request.abortTrigger != null) {
        request.abortTrigger!.then((_) {
          if (!abortObserved.isCompleted) abortObserved.complete();
        });
      }
      received409.complete();
      await release.future;
    }
    return response;
  }

  @override
  void close() => _inner.close();
}

class _HttpTraceAdapter extends MockCortexApi
    implements ComputerTraceApi, AbortableComputerTraceApi {
  _HttpTraceAdapter(this.api) : super(instant: true);
  final HttpCortexApi api;

  @override
  Future<ComputerUseTraceHistory> computerUseTraces(String sessionId) =>
      api.computerUseTraces(sessionId);

  @override
  Future<ComputerUseTraceHistory> computerUseTracesAbortable(
    String sessionId, {
    required Future<void> abortTrigger,
  }) => api.computerUseTracesAbortable(sessionId, abortTrigger: abortTrigger);
}

class _StaticChat extends ChatController {
  @override
  ChatState build() => const ChatState(activeSessionId: 'ses_trace_a');
}

class _StaticToolChat extends ChatController {
  @override
  ChatState build() {
    // 复用 ChatToolEvent 的合并规则构造工具状态，让真实查看器可打开。
    const event = ChatToolEvent(
      name: 'computer_observe',
      summary: '调用 computer_observe',
      phase: ToolPhase.call,
    );
    final turn = StreamingTurn(
      messageId: 'message-a',
      sessionId: 'ses_trace_a',
      startedAt: DateTime.utc(2026),
      toolCalls: ToolCall.merge(
        const [],
        event.name,
        event.summary,
        phase: event.phase,
      ),
    );
    return ChatState(
      activeSessionId: 'ses_trace_a',
      streamingTurns: {'ses_trace_a': turn},
    );
  }
}

class _EmptyConfirm extends ConfirmController {
  @override
  ConfirmState build() => const ConfirmState();
}

void main() {
  test('409 头已收到后先执行 abort 再放行，观察清理前服务端断开', () async {
    final server = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
    final connected = Completer<Socket>();
    final disconnected = Completer<String>();
    final serverSubscription = server.listen((socket) {
      if (!connected.isCompleted) connected.complete(socket);
      var request = '';
      var sent = false;
      socket.listen(
        (bytes) {
          request += latin1.decode(bytes);
          if (!sent && request.contains('\r\n\r\n')) {
            sent = true;
            socket.add(
              ascii.encode(
                'HTTP/1.1 409 Conflict\r\n'
                'Content-Type: application/json\r\n'
                'Content-Length: 1024\r\n'
                'Connection: keep-alive\r\n\r\n{',
              ),
            );
          }
        },
        onDone: () {
          if (!disconnected.isCompleted) disconnected.complete('FIN');
        },
        onError: (Object error) {
          if (!disconnected.isCompleted) {
            disconnected.complete('socket error: ${error.runtimeType}');
          }
        },
      );
    });
    final client = _BlockedHeadersClient();
    final api = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:${server.port}',
      client: client,
    );
    final abort = Completer<void>();
    try {
      final result = api.computerUseTracesAbortable(
        'ses_trace_a',
        abortTrigger: abort.future,
      );
      final check = expectLater(
        result,
        throwsA(
          isA<CortexApiException>().having((e) => e.statusCode, 'status', 409),
        ),
      );
      await client.received409.future.timeout(const Duration(seconds: 5));
      abort.complete();
      await client.abortObserved.future.timeout(const Duration(seconds: 5));
      client.release.complete();
      await check.timeout(const Duration(seconds: 5));
      final observation = await Future.any([
        disconnected.future,
        Future<String>.delayed(const Duration(seconds: 2), () => '2 秒内未观察到断开'),
      ]);
      debugPrint('409 已返回；清理前服务端连接观察：$observation');
    } finally {
      api.dispose();
      if (connected.isCompleted) (await connected.future).destroy();
      await serverSubscription.cancel();
      await server.close();
    }
  });

  test('普通 trace 在非 409 响应体遇到取消时包装为 CortexApiException', () async {
    final client = MockClient.streaming((request, _) async {
      return http.StreamedResponse(
        Stream<List<int>>.error(http.RequestAbortedException(request.url)),
        200,
      );
    });
    final api = HttpCortexApi(baseUrl: 'http://127.0.0.1:8080', client: client);
    addTearDown(api.dispose);
    await expectLater(
      api.computerUseTraces('ses_trace_a'),
      throwsA(
        isA<CortexApiException>()
            .having((error) => error.statusCode, 'status', isNull)
            .having(
              (error) => error.cause,
              'cause',
              isA<http.RequestAbortedException>(),
            ),
      ),
    );
  });

  test('真实 IOClient 可中止正在等待响应头的 trace 请求', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    final received = Completer<void>();
    final subscription = server.listen((request) {
      expect(request.uri.path, '/local/computer-traces/ses_trace_a');
      received.complete();
      // 故意不返回响应，验证取消能打断真实网络等待。
    });
    addTearDown(subscription.cancel);
    final api = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:${server.port}',
      client: IOClient(_loopbackHttpClient()),
    );
    addTearDown(api.dispose);
    final abort = Completer<void>();
    final response = api.computerUseTracesAbortable(
      'ses_trace_a',
      abortTrigger: abort.future,
    );
    await received.future.timeout(const Duration(seconds: 5));
    abort.complete();
    await expectLater(
      response.timeout(const Duration(seconds: 5)),
      throwsA(isA<http.RequestAbortedException>()),
    );
  });

  for (final abortable in [false, true]) {
    test(
      '真实 IOClient ${abortable ? '可取消' : '普通'}路径收到 409 头后刷新失败仍保留 409',
      () async {
        final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
        addTearDown(() => server.close(force: true));
        var calls = 0;
        final subscription = server.listen((request) async {
          calls++;
          if (calls == 1) {
            request.response.statusCode = 409;
            request.response.contentLength = 1024;
            request.response.bufferOutput = false;
            request.response.add(utf8.encode('{'));
            await request.response.flush();
            // 故意不结束响应体；测试结束时由 server.close(force: true) 清理。
            return;
          }
          request.response.statusCode = 503;
          request.response.headers.contentType = ContentType.json;
          request.response.write(
            jsonEncode({'error': '暂不可用', 'retryable': false}),
          );
          await request.response.close();
        });
        addTearDown(subscription.cancel);
        final client = _HeadersClient();
        final origin = 'http://127.0.0.1:${server.port}';
        final api = HttpCortexApi(baseUrl: origin, client: client);
        final refreshed = HttpCortexApi(
          baseUrl: origin,
          client: IOClient(_loopbackHttpClient()),
        );
        addTearDown(api.dispose);
        addTearDown(refreshed.dispose);
        final abort = Completer<void>();
        final first = abortable
            ? api.computerUseTracesAbortable(
                'ses_trace_a',
                abortTrigger: abort.future,
              )
            : api.computerUseTraces('ses_trace_a');
        final firstCheck = expectLater(
          first,
          throwsA(
            isA<CortexApiException>().having(
              (e) => e.statusCode,
              'status',
              409,
            ),
          ),
        );
        await client.received409.future.timeout(const Duration(seconds: 5));
        if (abortable) {
          abort.complete();
        } else {
          api.dispose();
        }
        await expectLater(
          refreshed.computerUseTraces('ses_trace_a'),
          throwsA(
            isA<CortexApiException>().having(
              (e) => e.statusCode,
              'status',
              503,
            ),
          ),
        );
        await firstCheck.timeout(const Duration(seconds: 5));
      },
    );
  }

  testWidgets('真实回环 409 响应体挂起后刷新 503，provider 仍显示缺口', (tester) async {
    late HttpServer server;
    await tester.runAsync(() async {
      server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    });
    addTearDown(() => server.close(force: true));
    final secondResponse = Completer<void>();
    var calls = 0;
    final subscription = server.listen((request) async {
      calls++;
      if (calls == 1) {
        request.response.statusCode = 409;
        request.response.contentLength = 1024;
        request.response.bufferOutput = false;
        request.response.add(utf8.encode('{'));
        await request.response.flush();
        return;
      }
      request.response.statusCode = 503;
      request.response.headers.contentType = ContentType.json;
      request.response.write(jsonEncode({'error': '重查失败', 'retryable': false}));
      await request.response.close();
      if (!secondResponse.isCompleted) secondResponse.complete();
    });
    addTearDown(subscription.cancel);
    final client = _HeadersClient();
    final httpApi = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:${server.port}',
      client: client,
    );
    addTearDown(httpApi.dispose);
    final adapter = _HttpTraceAdapter(httpApi);
    final container = ProviderContainer(
      overrides: [
        cortexApiProvider.overrideWithValue(adapter),
        chatControllerProvider.overrideWith(_StaticChat.new),
        confirmControllerProvider.overrideWith(_EmptyConfirm.new),
      ],
    );
    addTearDown(container.dispose);
    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: container,
        child: const MaterialApp(
          home: Scaffold(body: Center(child: ComputerUseTraceButton())),
        ),
      ),
    );
    for (var i = 0; i < 3; i++) {
      await tester.pump(const Duration(milliseconds: 20));
    }
    for (var i = 0; i < 100 && !client.received409.isCompleted; i++) {
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 10)),
      );
      await tester.pump(const Duration(milliseconds: 1));
    }
    expect(client.received409.isCompleted, isTrue, reason: '真实客户端必须收到 409 响应头');
    for (
      var i = 0;
      i < 100 &&
          find.byKey(const ValueKey('computer_use:trace')).evaluate().isEmpty;
      i++
    ) {
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 10)),
      );
      await tester.pump(const Duration(milliseconds: 1));
    }
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    for (var i = 0; i < 3; i++) {
      await tester.pump(const Duration(milliseconds: 20));
    }
    for (var i = 0; i < 100 && !secondResponse.isCompleted; i++) {
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 10)),
      );
      await tester.pump(const Duration(milliseconds: 1));
    }
    expect(secondResponse.isCompleted, isTrue, reason: '刷新请求必须收到 503 响应');
    for (
      var i = 0;
      i < 100 && find.textContaining('重查失败').evaluate().isEmpty;
      i++
    ) {
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 10)),
      );
      await tester.pump(const Duration(milliseconds: 1));
    }
    expect(find.textContaining('历史不完整'), findsOneWidget);
    expect(find.textContaining('重查失败'), findsOneWidget);
    expect(find.textContaining('无法据此判断操作是否发生'), findsOneWidget);
    await tester.pumpWidget(const SizedBox.shrink());
    container.dispose();
    httpApi.dispose();
    await tester.pump();
  });

  testWidgets('电脑工具事件可打开查看器，刷新后屏障中的旧 409 仍覆盖 503', (tester) async {
    late HttpServer server;
    await tester.runAsync(() async {
      server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    });
    var calls = 0;
    final secondResponse = Completer<void>();
    final serverSubscription = server.listen((request) async {
      calls++;
      if (calls == 1) {
        request.response.statusCode = 409;
        request.response.contentLength = 1024;
        request.response.bufferOutput = false;
        request.response.add(utf8.encode('{'));
        await request.response.flush();
        return;
      }
      request.response.statusCode = 503;
      request.response.headers.contentType = ContentType.json;
      request.response.write(jsonEncode({'error': '刷新失败', 'retryable': false}));
      await request.response.close();
      if (!secondResponse.isCompleted) secondResponse.complete();
    });
    final client = _BlockedHeadersClient();
    final httpApi = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:${server.port}',
      client: client,
    );
    final api = _HttpTraceAdapter(httpApi);
    final container = ProviderContainer(
      overrides: [
        cortexApiProvider.overrideWithValue(api),
        chatControllerProvider.overrideWith(_StaticToolChat.new),
        confirmControllerProvider.overrideWith(_EmptyConfirm.new),
      ],
    );
    var cleaned = false;
    addTearDown(() async {
      if (cleaned) return;
      if (!client.release.isCompleted) client.release.complete();
      container.dispose();
      httpApi.dispose();
      await server.close(force: true).timeout(const Duration(seconds: 3));
      await serverSubscription.cancel().timeout(const Duration(seconds: 3));
    });
    await tester.pumpWidget(
      UncontrolledProviderScope(
        container: container,
        child: const MaterialApp(
          home: Scaffold(body: Center(child: ComputerUseTraceButton())),
        ),
      ),
    );
    for (var i = 0; i < 6; i++) {
      await tester.pump(const Duration(milliseconds: 20));
    }
    expect(find.byKey(const ValueKey('computer_use:trace')), findsOneWidget);
    await tester.tap(find.byKey(const ValueKey('computer_use:trace')));
    for (var i = 0; i < 6; i++) {
      await tester.pump(const Duration(milliseconds: 20));
    }
    for (var i = 0; i < 200 && !client.received409.isCompleted; i++) {
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 10)),
      );
      await tester.pump(const Duration(milliseconds: 1));
    }
    expect(
      client.received409.isCompleted,
      isTrue,
      reason: '旧请求必须先收到 409 头并被屏障拦住',
    );
    await tester.tap(find.byKey(const ValueKey('computer_use:trace-refresh')));
    for (
      var i = 0;
      i < 200 &&
          (!client.abortObserved.isCompleted || !secondResponse.isCompleted);
      i++
    ) {
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 10)),
      );
      await tester.pump(const Duration(milliseconds: 1));
    }
    expect(client.abortObserved.isCompleted, isTrue, reason: '刷新必须先触发旧请求的取消信号');
    expect(secondResponse.isCompleted, isTrue, reason: '新请求必须先收到 503');
    for (
      var i = 0;
      i < 100 && find.textContaining('刷新失败').evaluate().isEmpty;
      i++
    ) {
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 10)),
      );
      await tester.pump(const Duration(milliseconds: 1));
    }
    expect(find.textContaining('刷新失败'), findsOneWidget);
    client.release.complete();
    for (
      var i = 0;
      i < 100 && find.textContaining('历史不完整').evaluate().isEmpty;
      i++
    ) {
      await tester.runAsync(
        () => Future<void>.delayed(const Duration(milliseconds: 10)),
      );
      await tester.pump(const Duration(milliseconds: 1));
    }
    expect(
      find.textContaining('历史不完整'),
      findsOneWidget,
      reason: '旧 409 迟到后仍是缺口证据',
    );
    expect(find.textContaining('刷新失败'), findsOneWidget);
    await tester.pumpWidget(const SizedBox.shrink());
    container.dispose();
    httpApi.dispose();
    await tester.runAsync(() async {
      await server.close(force: true).timeout(const Duration(seconds: 3));
      await serverSubscription.cancel().timeout(const Duration(seconds: 3));
    });
    cleaned = true;
    await tester.pump();
  });
}
