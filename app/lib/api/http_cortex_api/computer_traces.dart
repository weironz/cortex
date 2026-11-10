part of '../http_cortex_api.dart';

mixin _ComputerTraceEndpoints on HttpCortexApiBase
    implements ComputerTraceApi, AbortableComputerTraceApi {
  @override
  Future<ComputerUseTraceHistory> computerUseTraces(String sessionId) =>
      _computerUseTraces(sessionId);

  @override
  Future<ComputerUseTraceHistory> computerUseTracesAbortable(
    String sessionId, {
    required Future<void> abortTrigger,
  }) => _computerUseTraces(sessionId, abortTrigger: abortTrigger);

  Future<ComputerUseTraceHistory> _computerUseTraces(
    String sessionId, {
    Future<void>? abortTrigger,
  }) async {
    if (!_safeTraceSessionId(sessionId)) {
      throw const CortexApiException('会话 id 不合法。', statusCode: 400);
    }
    final path = '/local/computer-traces/${Uri.encodeComponent(sessionId)}';
    const query = {'include_denials': 'true'};
    final request = abortTrigger == null
        ? http.Request('GET', _uri(path, query))
        : http.AbortableRequest(
            'GET',
            _uri(path, query),
            abortTrigger: abortTrigger,
          );
    request.headers.addAll(_headers(const {'accept': 'application/json'}));
    final http.StreamedResponse streamed;
    try {
      streamed = await _client.send(request);
    } on http.RequestAbortedException catch (error) {
      if (abortTrigger != null) rethrow;
      throw CortexApiException(_unreachableMessage(error), cause: error);
    } on Object catch (error) {
      throw CortexApiException(_unreachableMessage(error), cause: error);
    }
    if (streamed.statusCode == 409) {
      // 状态头已经证明有缺口；不能等可能永不结束或被刷新中断的响应体。
      // 不读取 body 就不知道 retryable 与服务端文案，用固定提示且不猜该字段。
      try {
        final subscription = streamed.stream.listen(
          (_) {},
          onError: (Object _) {},
        );
        unawaited(subscription.cancel().catchError((Object _) {}));
      } on Object {
        // 关闭响应流失败也不能抹掉已经收到的缺口状态。
      }
      throw _failure(
        409,
        '该会话 Computer Use 历史不完整；请人工核对',
        headers: streamed.headers,
      );
    }
    final http.Response response;
    try {
      response = await http.Response.fromStream(streamed);
    } on http.RequestAbortedException catch (error) {
      if (abortTrigger != null) rethrow;
      throw CortexApiException(_unreachableMessage(error), cause: error);
    } on Object catch (error) {
      throw CortexApiException(_unreachableMessage(error), cause: error);
    }
    if (response.statusCode >= 400) {
      throw _failure(
        response.statusCode,
        _errorMessage(response),
        headers: response.headers,
        retryable: _unwrapRetryable(utf8.decode(response.bodyBytes)),
      );
    }
    final json = _decodeObject(
      path,
      utf8.decode(response.bodyBytes),
      _decodeComputerTraceJson,
    );
    try {
      return ComputerUseTraceHistory.fromJson(json);
    } on FormatException catch (error) {
      throw CortexApiException(
        'Computer Use trace 响应不符合只读协议：${error.message}',
        statusCode: 502,
        cause: error,
      );
    }
  }
}

final _jsonNumber = RegExp(
  r'-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?',
);
final _unsignedInteger = RegExp(r'^(?:0|[1-9][0-9]*)$');
final _maxSafeJsonInteger = BigInt.parse('9007199254740991');

/// Dart 的默认 JSON 解码器会在 Web 上舍入大整数，在原生端把超出 i64 的
/// 整数变成 double。先把数字替换成索引，交给标准解码器校验结构，再还原
/// 两个 u64 字段的原始数字；字符串始终原样交给标准解码器处理。
Object? _decodeComputerTraceJson(String source) {
  final indexed = StringBuffer();
  final numbers = <String>[];
  var inString = false;
  var escaped = false;
  var index = 0;
  while (index < source.length) {
    final code = source.codeUnitAt(index);
    if (inString) {
      indexed.writeCharCode(code);
      if (escaped) {
        escaped = false;
      } else if (code == 0x5c) {
        escaped = true;
      } else if (code == 0x22) {
        inString = false;
      }
      index++;
      continue;
    }
    if (code == 0x22) {
      inString = true;
      indexed.writeCharCode(code);
      index++;
      continue;
    }
    if (code == 0x2d || (code >= 0x30 && code <= 0x39)) {
      final match = _jsonNumber.matchAsPrefix(source, index);
      if (match != null) {
        final end = match.end;
        if (end < source.length &&
            !const {
              0x20,
              0x09,
              0x0a,
              0x0d,
              0x2c,
              0x5d,
              0x7d,
            }.contains(source.codeUnitAt(end))) {
          throw const FormatException('JSON 数字格式不合法');
        }
        numbers.add(match.group(0)!);
        indexed.write(numbers.length - 1);
        index = end;
        continue;
      }
    }
    indexed.writeCharCode(code);
    index++;
  }

  return jsonDecode(
    indexed.toString(),
    reviver: (key, value) {
      if (value is! int) return value;
      final number = numbers[value];
      if (key == 'sequence' || key == 'timestamp_unix_ms') {
        // 非整数词法留给协议校验拒绝；不能把 1.0 或 1e0 当作 u64。
        if (!_unsignedInteger.hasMatch(number)) return number;
        // 超过 20 位已经不可能是 u64，无需为超长输入构造大整数。
        if (number.length > 20) return BigInt.parse('18446744073709551616');
        final integer = BigInt.parse(number);
        return integer <= _maxSafeJsonInteger ? integer.toInt() : integer;
      }
      return jsonDecode(number);
    },
  );
}

bool _safeTraceSessionId(String value) {
  if (value.isEmpty || value.length > 128) return false;
  return RegExp(r'^[A-Za-z0-9_-]+$').hasMatch(value);
}
