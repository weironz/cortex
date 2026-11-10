import 'dart:async';
import 'dart:convert';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/http_cortex_api.dart';
import 'package:cortex_app/core/computer_input_recovery_api.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:http/http.dart' as http;
import 'package:http/testing.dart';

class _PendingRecoveryClient extends http.BaseClient {
  final entered = Completer<void>();
  final response = Completer<http.StreamedResponse>();
  bool aborted = false;

  @override
  Future<http.StreamedResponse> send(http.BaseRequest request) {
    expect(request, isA<http.AbortableRequest>());
    final abort = (request as http.AbortableRequest).abortTrigger;
    if (abort != null) {
      unawaited(
        abort.then((_) {
          aborted = true;
          if (!response.isCompleted) {
            response.completeError(http.RequestAbortedException(request.url));
          }
        }),
      );
    }
    entered.complete();
    return response.future;
  }
}

void main() {
  test('恢复 DTO 精确解析六阶段，未知/缺字段默认不可用', () {
    const phases = {
      'unavailable': ComputerInputRecoveryPhase.unavailable,
      'needs_stop': ComputerInputRecoveryPhase.needsStop,
      'working': ComputerInputRecoveryPhase.working,
      'needs_review': ComputerInputRecoveryPhase.needsReview,
      'complete': ComputerInputRecoveryPhase.complete,
      'failed': ComputerInputRecoveryPhase.failed,
    };
    for (final entry in phases.entries) {
      final state = ComputerInputRecoveryState.fromJson({
        'phase': entry.key,
        'recovery_id': 'opaque-recovery-v1',
        'operation_id': 'A01f',
        'epoch': 7,
        'message': '请人工核对外部结果',
      });
      expect(state.phase, entry.value);
      expect(state.recoveryId, 'opaque-recovery-v1');
      expect(state.operationId, 'A01f');
      expect(state.epoch, 7);
    }
    for (final phase in <Object?>[null, 'READY', 'future', true, 1]) {
      expect(
        ComputerInputRecoveryState.fromJson({'phase': phase}).phase,
        ComputerInputRecoveryPhase.unavailable,
      );
    }
    expect(
      ComputerInputRecoveryState.fromJson(jsonDecode('{}')).phase,
      ComputerInputRecoveryPhase.unavailable,
    );
    for (final raw in <Object?>[null, [], true, 1, '{"phase":"complete"}']) {
      expect(
        () => ComputerInputRecoveryState.fromJson(raw),
        throwsFormatException,
        reason: '非对象响应必须作为协议错误拒绝，不能冒充有效恢复状态',
      );
    }
    for (final epoch in <Object>[-1, 1.0, '7']) {
      expect(
        () => ComputerInputRecoveryState.fromJson({
          'phase': 'needs_stop',
          'epoch': epoch,
        }),
        throwsFormatException,
      );
    }
    expect(
      () => ComputerInputRecoveryState.fromJson({
        'phase': 'needs_review',
        'operation_id': 'not-hex',
      }),
      throwsFormatException,
    );
  });

  test('HTTP 使用当前 session 与专用恢复路径，ack 固定 reviewed，不走工具确认', () async {
    final requests = <http.Request>[];
    final api = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:48000',
      token: 'test-local-control',
      client: MockClient((request) async {
        requests.add(request);
        return http.Response(
          jsonEncode({
            'phase': request.url.path.endsWith('acknowledge')
                ? 'complete'
                : 'needs_review',
            'recovery_id': 'opaque-id',
            'operation_id': 'ab12',
            'epoch': 2,
          }),
          200,
        );
      }),
    );
    addTearDown(api.dispose);
    final recovery = api as ComputerInputRecoveryApi;
    await recovery.inputRecovery('existing-session');
    await recovery.retireInputRecovery(
      'existing-session',
      recoveryId: 'opaque-id',
    );
    final result = await recovery.acknowledgeInputRecovery(
      'existing-session',
      recoveryId: 'opaque-id',
    );
    expect(result.phase, ComputerInputRecoveryPhase.complete);
    expect(requests.map((r) => r.method), ['GET', 'POST', 'POST']);
    expect(requests.map((r) => r.url.path), [
      '/local/computer-input/recovery',
      '/local/computer-input/recovery/retire',
      '/local/computer-input/recovery/acknowledge',
    ]);
    expect(requests.first.url.queryParameters, {
      'session_id': 'existing-session',
    });
    expect(jsonDecode(requests[1].body), {
      'session_id': 'existing-session',
      'recovery_id': 'opaque-id',
    });
    expect(jsonDecode(requests[2].body), {
      'session_id': 'existing-session',
      'recovery_id': 'opaque-id',
      'result': 'reviewed',
    });
    expect(
      requests.every(
        (r) => r.headers['authorization'] == 'Bearer test-local-control',
      ),
      isTrue,
    );
    expect(requests.every((r) => !r.followRedirects), isTrue);
  });

  test('404 只能表示不可用，202 必须为 working，错误体不能冒充 complete', () async {
    for (final status in [200, 202, 404, 401, 403, 409, 500]) {
      final api = HttpCortexApi(
        baseUrl: 'http://127.0.0.1:48000',
        client: MockClient(
          (_) async => http.Response('{"phase":"complete"}', status),
        ),
      );
      addTearDown(api.dispose);
      final future = api.inputRecovery('existing-session');
      if (status == 200) {
        expect((await future).phase, ComputerInputRecoveryPhase.complete);
      } else if (status == 404) {
        expect((await future).phase, ComputerInputRecoveryPhase.unavailable);
      } else {
        await expectLater(future, throwsA(isA<CortexApiException>()));
      }
    }
    final api = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:48000',
      client: MockClient(
        (_) async => http.Response('{"phase":"working"}', 202),
      ),
    );
    addTearDown(api.dispose);
    expect(
      (await api.inputRecovery('existing-session')).phase,
      ComputerInputRecoveryPhase.working,
    );
  });

  test('恢复请求保留取消信号，取消不被包装成成功', () async {
    final client = _PendingRecoveryClient();
    final api = HttpCortexApi(
      baseUrl: 'http://127.0.0.1:48000',
      client: client,
    );
    addTearDown(api.dispose);
    final abort = Completer<void>();
    final result = api.inputRecovery(
      'existing-session',
      abortTrigger: abort.future,
    );
    final expectation = expectLater(
      result,
      throwsA(isA<http.RequestAbortedException>()),
    );
    await client.entered.future;
    abort.complete();
    await expectation;
    expect(client.aborted, isTrue);
  });
}
