/// 只供用户设置界面的输入恢复控制面，不属于模型工具或通用工具确认。
library;

import 'package:flutter/foundation.dart' show kIsWeb;

/// abortTrigger 只取消客户端等待/传输，不表示服务端已停止或回滚恢复流程。
abstract interface class ComputerInputRecoveryApi {
  Future<ComputerInputRecoveryState> inputRecovery(
    String sessionId, {
    Future<void>? abortTrigger,
  });

  Future<ComputerInputRecoveryState> retireInputRecovery(
    String sessionId, {
    required String recoveryId,
    Future<void>? abortTrigger,
  });

  Future<ComputerInputRecoveryState> acknowledgeInputRecovery(
    String sessionId, {
    required String recoveryId,
    Future<void>? abortTrigger,
  });
}

enum ComputerInputRecoveryPhase {
  unavailable,
  needsStop,
  working,
  needsReview,
  complete,
  failed,
}

class ComputerInputRecoveryState {
  const ComputerInputRecoveryState({
    this.phase = ComputerInputRecoveryPhase.unavailable,
    this.recoveryId,
    this.operationId,
    this.epoch,
    this.message,
  });

  final ComputerInputRecoveryPhase phase;
  final String? recoveryId;
  final String? operationId;
  final int? epoch;
  final String? message;

  factory ComputerInputRecoveryState.fromJson(Object? raw) {
    if (raw is! Map<String, dynamic>) {
      throw const FormatException('输入恢复响应不是对象');
    }
    final phase = switch (raw['phase']) {
      'unavailable' => ComputerInputRecoveryPhase.unavailable,
      'needs_stop' => ComputerInputRecoveryPhase.needsStop,
      'working' => ComputerInputRecoveryPhase.working,
      'needs_review' => ComputerInputRecoveryPhase.needsReview,
      'complete' => ComputerInputRecoveryPhase.complete,
      'failed' => ComputerInputRecoveryPhase.failed,
      _ => null,
    };
    if (phase == null) {
      return const ComputerInputRecoveryState(
        message: '当前后端未提供可识别的输入恢复状态，输入保持暂停。',
      );
    }
    final recoveryId = _optionalString(raw, 'recovery_id', 1024);
    final operationId = _optionalString(raw, 'operation_id', 256);
    if (operationId != null &&
        !RegExp(r'^[0-9a-fA-F]+$').hasMatch(operationId)) {
      throw const FormatException('操作编号不是十六进制摘要');
    }
    final epoch = switch (raw['epoch']) {
      null => null,
      int value when value >= 0 && (!kIsWeb || value <= 9007199254740991) =>
        value,
      _ => throw const FormatException('输入代际不是可精确表示的非负整数'),
    };
    return ComputerInputRecoveryState(
      phase: phase,
      recoveryId: recoveryId,
      operationId: operationId,
      epoch: epoch,
      message: _optionalString(raw, 'message', 2000),
    );
  }

  static String? _optionalString(
    Map<String, dynamic> json,
    String key,
    int maximum,
  ) {
    final value = json[key];
    if (value == null) return null;
    if (key == 'message' && value is String && value.trim().isEmpty) {
      return null;
    }
    if (value is! String || value.isEmpty || value.length > maximum) {
      throw FormatException('输入恢复 $key 格式不合法');
    }
    return value;
  }
}
