part of '../http_cortex_api.dart';

mixin _ComputerInputRecoveryEndpoints on HttpCortexApiBase
    implements ComputerInputRecoveryApi {
  static const _recoveryPath = '/local/computer-input/recovery';

  @override
  Future<ComputerInputRecoveryState> inputRecovery(
    String sessionId, {
    Future<void>? abortTrigger,
  }) => _inputRecoveryRequest(
    'GET',
    _recoveryPath,
    sessionId,
    abortTrigger: abortTrigger,
  );

  @override
  Future<ComputerInputRecoveryState> retireInputRecovery(
    String sessionId, {
    required String recoveryId,
    Future<void>? abortTrigger,
  }) => _inputRecoveryRequest(
    'POST',
    '$_recoveryPath/retire',
    sessionId,
    recoveryId: recoveryId,
    abortTrigger: abortTrigger,
  );

  @override
  Future<ComputerInputRecoveryState> acknowledgeInputRecovery(
    String sessionId, {
    required String recoveryId,
    Future<void>? abortTrigger,
  }) => _inputRecoveryRequest(
    'POST',
    '$_recoveryPath/acknowledge',
    sessionId,
    recoveryId: recoveryId,
    reviewed: true,
    abortTrigger: abortTrigger,
  );

  Future<ComputerInputRecoveryState> _inputRecoveryRequest(
    String method,
    String path,
    String sessionId, {
    String? recoveryId,
    bool reviewed = false,
    Future<void>? abortTrigger,
  }) async {
    if (sessionId.isEmpty ||
        sessionId.trim() != sessionId ||
        sessionId.length > 512 ||
        RegExp(r'[\x00-\x1f\x7f]').hasMatch(sessionId)) {
      throw const CortexApiException('请先打开对应账号下已有的会话。', statusCode: 400);
    }
    if (method == 'POST' &&
        (recoveryId == null ||
            recoveryId.isEmpty ||
            recoveryId.length > 1024)) {
      throw const CortexApiException('恢复凭据不可用，请重新核对状态。', statusCode: 400);
    }
    final uri = _uri(path, method == 'GET' ? {'session_id': sessionId} : null);
    final http.Request request = abortTrigger == null
        ? http.Request(method, uri)
        : http.AbortableRequest(method, uri, abortTrigger: abortTrigger);
    request.followRedirects = false;
    request.headers.addAll(
      _headers({
        'accept': 'application/json',
        'cache-control': 'no-cache',
        if (method == 'POST') 'content-type': 'application/json',
      }),
    );
    if (method == 'POST') {
      request.body = jsonEncode({
        'session_id': sessionId,
        'recovery_id': recoveryId,
        if (reviewed) 'result': 'reviewed',
      });
    }
    final http.Response response;
    try {
      response = await http.Response.fromStream(await _client.send(request));
    } on http.RequestAbortedException {
      rethrow;
    } on Object catch (error) {
      throw CortexApiException('无法连接本机输入恢复入口。', cause: error);
    }
    if (response.statusCode == 404) {
      return const ComputerInputRecoveryState(
        message: '当前会话的输入恢复入口不可用，输入保持暂停。',
      );
    }
    if (response.statusCode >= 400) {
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
        retryable: _unwrapRetryable(utf8.decode(response.bodyBytes)),
      );
    }
    if (response.statusCode != 200 && response.statusCode != 202) {
      throw const CortexApiException('输入恢复入口返回了无法确认的响应。', statusCode: 502);
    }
    try {
      final state = ComputerInputRecoveryState.fromJson(
        jsonDecode(utf8.decode(response.bodyBytes)),
      );
      if (response.statusCode == 202 &&
          state.phase != ComputerInputRecoveryPhase.working) {
        throw const FormatException('202 必须报告 working，不能宣称恢复完成');
      }
      return state;
    } on FormatException catch (error) {
      throw CortexApiException(
        '无法确认本机输入恢复状态，输入保持暂停。',
        statusCode: 502,
        cause: error,
      );
    }
  }
}
