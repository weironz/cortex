/// 本地 Computer Use trace 的只读线协议与受限视图模型。
///
/// 服务端会先把 JSONL 重新序列化成 DTO；这里再按同一组字段名解析。界面只
/// 接收标题和字符串字段，不保留原始 JSON map，因此未知键、原始截图引用或
/// 后续误加的自由文本不会被顺手渲染。
library;

import 'package:flutter/foundation.dart' show kIsWeb;

abstract interface class ComputerTraceApi {
  Future<ComputerUseTraceHistory> computerUseTraces(String sessionId);
}

/// 只有底层实现支持逐请求取消时才实现；取消信号不代表服务端停止执行。
abstract interface class AbortableComputerTraceApi {
  Future<ComputerUseTraceHistory> computerUseTracesAbortable(
    String sessionId, {
    required Future<void> abortTrigger,
  });
}

class ComputerUseTraceHistory {
  const ComputerUseTraceHistory({required this.traces});

  final List<ComputerUseTrace> traces;

  factory ComputerUseTraceHistory.fromJson(Object? raw) {
    final json = _map(raw, 'trace history');
    final traces = _list(json['traces'], 'traces');
    return ComputerUseTraceHistory(
      traces: [for (final trace in traces) ComputerUseTrace.fromJson(trace)],
    );
  }
}

class ComputerUseTrace {
  const ComputerUseTrace({
    required this.runId,
    required this.sessionId,
    required this.policyVersion,
    required this.outcome,
    required this.fields,
    required this.events,
  });

  final String runId;
  final String sessionId;
  final String policyVersion;
  final String outcome;
  final List<ComputerUseTraceField> fields;
  final List<ComputerUseTraceEvent> events;

  factory ComputerUseTrace.fromJson(Object? raw) {
    final json = _map(raw, 'trace');
    final schemaVersion = _int(json['schema_version'], 'schema_version');
    if (schemaVersion != 1 && schemaVersion != 2) {
      throw FormatException('不支持的 Computer Use trace schema: $schemaVersion');
    }

    if (!json.containsKey('app')) {
      throw const FormatException('trace 缺少 app');
    }
    final appFields = <ComputerUseTraceField>[];
    if (json['app'] != null || schemaVersion == 1) {
      final app = _map(json['app'], 'app');
      final appKind = _enumName(app['kind'], const {
        'windows_packaged',
        'windows_executable',
        'mac_bundle',
        'linux_desktop',
        'browser_context',
      }, 'app.kind');
      appFields.addAll([
        ComputerUseTraceField('应用类型', appKind),
        ComputerUseTraceField(
          '稳定应用身份',
          _string(app['stable_id'], 'app.stable_id'),
        ),
        ComputerUseTraceField(
          '发布者已验证',
          _bool(app['publisher_verified'], 'app.publisher_verified')
              ? '是'
              : '否',
        ),
      ]);
    }
    final redaction = _map(json['redaction'], 'redaction');
    final reducer = _enumName(redaction['raw_screenshot_frames'], const {
      'omitted',
      'hash_only',
    }, 'redaction.raw_screenshot_frames');
    final typed = _enumName(redaction['typed_text'], const {
      'omitted',
      'hash_only',
    }, 'redaction.typed_text');
    final secure = _enumName(redaction['ax_secure_values'], const {
      'omitted',
      'hash_only',
    }, 'redaction.ax_secure_values');
    final credentials = _enumName(redaction['cookies_tokens_passwords'], const {
      'omitted',
      'hash_only',
    }, 'redaction.cookies_tokens_passwords');
    final clipboard = _enumName(redaction['clipboard_contents'], const {
      'omitted',
      'hash_only',
    }, 'redaction.clipboard_contents');
    final outcome = _enumName(json['outcome'], const {
      'in_progress',
      'observed_only',
      'completed',
      'verification_failed',
      'aborted',
    }, 'outcome');
    final events = [
      for (final event in _list(json['events'], 'events'))
        ComputerUseTraceEvent.fromJson(event, schemaVersion: schemaVersion),
    ];
    if (events.any((event) => event.type == 'pre_dispatch_rejection') &&
        outcome != 'aborted') {
      throw const FormatException('派发前拒绝的 trace 必须是 aborted');
    }

    return ComputerUseTrace(
      runId: _string(json['run_id'], 'run_id'),
      sessionId: _string(json['session_id'], 'session_id'),
      policyVersion: _string(json['policy_version'], 'policy_version'),
      outcome: outcome,
      fields: [
        ...appFields,
        ComputerUseTraceField('截图脱敏', reducer),
        ComputerUseTraceField('输入文字脱敏', typed),
        ComputerUseTraceField('安全字段脱敏', secure),
        ComputerUseTraceField('凭据脱敏', credentials),
        ComputerUseTraceField('剪贴板脱敏', clipboard),
      ],
      events: events,
    );
  }
}

class ComputerUseTraceField {
  const ComputerUseTraceField(this.label, this.value);

  final String label;
  final String value;
}

class ComputerUseTraceEvent {
  const ComputerUseTraceEvent({
    required this.sequence,
    required this.type,
    required this.title,
    required this.fields,
  });

  /// 常见值为 int；超过 Web 精确整数范围时保留为 BigInt。
  final Object sequence;
  final String type;
  final String title;
  final List<ComputerUseTraceField> fields;

  factory ComputerUseTraceEvent.fromJson(Object? raw, {int schemaVersion = 1}) {
    final json = _map(raw, 'event');
    final type = _enumName(json['type'], {
      'observation',
      'policy_decision',
      'confirmation',
      'action',
      'receipt',
      'verification',
      'aborted',
      if (schemaVersion == 2) 'pre_dispatch_rejection',
    }, 'event.type');
    final sequence = _uint64(json['sequence'], 'event.sequence');
    final fields = <ComputerUseTraceField>[];

    switch (type) {
      case 'pre_dispatch_rejection':
        final tool = _string(json['tool'], 'event.tool');
        const allowedTools = {
          'computer_act',
          'computer_browser_act',
          'computer_browser_open',
          'computer_browser_observe',
          'computer_browser_wait',
          'computer_list',
          'computer_observe',
          'computer_wait',
        };
        if (!allowedTools.contains(tool)) {
          // 异常会显示在历史视图里，不把未知工具名原样带到界面。
          throw const FormatException('event.tool 含未知值');
        }
        final timestamp = _uint64(
          json['timestamp_unix_ms'],
          'event.timestamp_unix_ms',
        );
        fields
          ..add(ComputerUseTraceField('工具', tool))
          ..add(ComputerUseTraceField('拒绝时间（UTC）', _timestamp(timestamp)));
      case 'observation':
        _addObservation(
          fields,
          _map(json['observation'], 'event.observation'),
          '观察',
        );
      case 'policy_decision':
        fields.add(
          ComputerUseTraceField(
            '策略决定',
            _enumName(json['decision'], const {
              'allow',
              'confirm_once',
              'confirm_every_time',
              'handoff_to_user',
              'deny',
            }, 'event.decision'),
          ),
        );
        _addAction(fields, _map(json['action'], 'event.action'), '策略动作');
      case 'confirmation':
        final confirmation = _map(json['confirmation'], 'event.confirmation');
        fields
          ..add(
            ComputerUseTraceField(
              '预览指纹',
              _string(confirmation['preview_hash'], 'preview_hash'),
            ),
          )
          ..add(
            ComputerUseTraceField(
              '确认结果',
              _enumName(confirmation['decision'], const {
                'approved',
                'rejected',
                'timed_out',
                'not_required',
                'handed_off',
              }, 'confirmation.decision'),
            ),
          );
      case 'action':
        fields.add(
          ComputerUseTraceField(
            '动作 id',
            '${_int(json['action_id'], 'event.action_id')}',
          ),
        );
        _addAction(fields, _map(json['summary'], 'event.summary'), '动作');
      case 'receipt':
        fields.add(
          ComputerUseTraceField(
            '动作 id',
            '${_int(json['action_id'], 'event.action_id')}',
          ),
        );
        _addReceipt(fields, _map(json['receipt'], 'event.receipt'));
      case 'verification':
        _addVerification(
          fields,
          _map(json['verification'], 'event.verification'),
        );
      case 'aborted':
        fields.add(
          ComputerUseTraceField(
            '中止原因',
            _enumName(json['reason'], const {
              'user_cancelled',
              'policy_denied',
              'driver_failure',
              'watchdog_timeout',
              'run_lease_lost',
            }, 'event.reason'),
          ),
        );
      default:
        throw FormatException('未知 Computer Use trace event: $type');
    }

    return ComputerUseTraceEvent(
      sequence: sequence,
      type: type,
      title: _eventTitle(type),
      fields: List.unmodifiable(fields),
    );
  }
}

void _addObservation(
  List<ComputerUseTraceField> fields,
  Map<String, dynamic> json,
  String prefix,
) {
  fields
    ..add(
      ComputerUseTraceField(
        '$prefix frame hash',
        _string(json['frame_hash'], '$prefix.frame_hash'),
      ),
    )
    ..add(
      ComputerUseTraceField(
        '$prefix 窗口',
        _string(json['window_ref'], '$prefix.window_ref'),
      ),
    )
    ..add(
      ComputerUseTraceField(
        '$prefix AX 摘要 hash',
        _nullableString(json['accessibility_summary_hash']),
      ),
    )
    ..add(
      ComputerUseTraceField(
        '$prefix 截图 hash',
        _nullableString(json['screenshot_hash']),
      ),
    );
}

void _addAction(
  List<ComputerUseTraceField> fields,
  Map<String, dynamic> json,
  String prefix,
) {
  final kind = _enumName(json['kind'], const {
    'batch',
    'click',
    'type_text',
    'key_chord',
    'scroll',
    'navigate',
    'paste_clipboard',
    'secure_field_handoff',
    'credential_handoff',
    'open_application',
    'wait',
  }, '$prefix.kind');
  fields.add(ComputerUseTraceField('$prefix 类别', kind));
  switch (kind) {
    case 'batch':
      final actions = _list(json['actions'], '$prefix.actions');
      for (var index = 0; index < actions.length; index++) {
        _addAction(
          fields,
          _map(actions[index], '$prefix.actions[$index]'),
          '$prefix ${index + 1}',
        );
      }
    case 'click':
      fields
        ..add(
          ComputerUseTraceField(
            '$prefix 目标',
            _string(json['target_ref'], '$prefix.target_ref'),
          ),
        )
        ..add(
          ComputerUseTraceField(
            '$prefix 按钮',
            _enumName(json['button'], const {
              'left',
              'right',
              'middle',
            }, '$prefix.button'),
          ),
        );
    case 'type_text':
      fields
        ..add(
          ComputerUseTraceField(
            '$prefix 目标',
            _string(json['target_ref'], '$prefix.target_ref'),
          ),
        )
        ..add(
          ComputerUseTraceField(
            '$prefix 输入文字',
            _redacted(json['text'], '$prefix.text'),
          ),
        );
    case 'key_chord':
      fields
        ..add(
          ComputerUseTraceField(
            '$prefix 目标',
            _nullableString(json['target_ref']),
          ),
        )
        ..add(
          ComputerUseTraceField(
            '$prefix 快捷键',
            _redacted(json['keys'], '$prefix.keys'),
          ),
        );
    case 'scroll':
      fields
        ..add(
          ComputerUseTraceField(
            '$prefix 目标',
            _string(json['target_ref'], '$prefix.target_ref'),
          ),
        )
        ..add(
          ComputerUseTraceField(
            '$prefix 方向',
            _enumName(json['direction'], const {
              'up',
              'down',
              'left',
              'right',
            }, '$prefix.direction'),
          ),
        )
        ..add(
          ComputerUseTraceField(
            '$prefix 数量',
            '${_int(json['amount'], '$prefix.amount')}',
          ),
        );
    case 'navigate':
      fields.add(
        ComputerUseTraceField(
          '$prefix URL 指纹',
          _string(json['url_hash'], '$prefix.url_hash'),
        ),
      );
    case 'paste_clipboard':
      fields
        ..add(
          ComputerUseTraceField(
            '$prefix 目标',
            _string(json['target_ref'], '$prefix.target_ref'),
          ),
        )
        ..add(
          ComputerUseTraceField(
            '$prefix 剪贴板',
            _redacted(json['contents'], '$prefix.contents'),
          ),
        );
    case 'secure_field_handoff':
      fields
        ..add(
          ComputerUseTraceField(
            '$prefix 目标',
            _string(json['target_ref'], '$prefix.target_ref'),
          ),
        )
        ..add(
          ComputerUseTraceField(
            '$prefix 安全值',
            _redacted(json['secure_value'], '$prefix.secure_value'),
          ),
        );
    case 'credential_handoff':
      fields
        ..add(
          ComputerUseTraceField(
            '$prefix 目标',
            _string(json['target_ref'], '$prefix.target_ref'),
          ),
        )
        ..add(
          ComputerUseTraceField(
            '$prefix 凭据类别',
            _enumName(json['credential'], const {
              'cookie',
              'token',
              'password',
            }, '$prefix.credential'),
          ),
        )
        ..add(
          ComputerUseTraceField(
            '$prefix 凭据值',
            _redacted(json['value'], '$prefix.value'),
          ),
        );
    case 'open_application':
      fields
        ..add(
          ComputerUseTraceField(
            '$prefix 应用',
            _string(json['app_id'], '$prefix.app_id'),
          ),
        )
        ..add(
          ComputerUseTraceField(
            '$prefix URL 指纹',
            _nullableString(json['url_hash']),
          ),
        );
    case 'wait':
      fields.add(
        ComputerUseTraceField(
          '$prefix 条件指纹',
          _string(json['condition_hash'], '$prefix.condition_hash'),
        ),
      );
  }
}

void _addReceipt(
  List<ComputerUseTraceField> fields,
  Map<String, dynamic> json,
) {
  fields
    ..add(
      ComputerUseTraceField(
        '回执 id',
        _string(json['receipt_id'], 'receipt.receipt_id'),
      ),
    )
    ..add(
      ComputerUseTraceField(
        '回执状态',
        _enumName(json['status'], const {
          'accepted',
          'completed',
          'failed',
        }, 'receipt.status'),
      ),
    )
    ..add(
      ComputerUseTraceField(
        '耗时',
        '${_int(json['duration_ms'], 'receipt.duration_ms')} ms',
      ),
    )
    ..add(
      ComputerUseTraceField(
        '失败类别',
        _nullableEnumName(json['failure'], const {
          'driver_timeout',
          'input_rejected',
          'window_lost',
          'other',
        }, 'receipt.failure'),
      ),
    );
}

void _addVerification(
  List<ComputerUseTraceField> fields,
  Map<String, dynamic> json,
) {
  fields.add(
    ComputerUseTraceField(
      '验证动作 id',
      '${_int(json['action_id'], 'verification.action_id')}',
    ),
  );
  final check = _map(json['check'], 'verification.check');
  final kind = _enumName(check['kind'], const {
    'element_present',
    'element_absent',
    'element_state_hash',
    'window_summary_hash',
  }, 'verification.check.kind');
  final kindLabel = switch (kind) {
    'element_state_hash' => '元素状态指纹',
    'window_summary_hash' => '窗口摘要指纹',
    _ => kind,
  };
  fields.add(ComputerUseTraceField('验证条件', kindLabel));
  switch (kind) {
    case 'element_present':
    case 'element_absent':
      fields.add(
        ComputerUseTraceField(
          '验证目标',
          _string(check['target_ref'], 'verification.check.target_ref'),
        ),
      );
    case 'element_state_hash':
      fields
        ..add(
          ComputerUseTraceField(
            '验证目标',
            _string(check['target_ref'], 'verification.check.target_ref'),
          ),
        )
        ..add(
          ComputerUseTraceField(
            '期望状态指纹',
            _string(
              check['expected_state_hash'],
              'verification.check.expected_state_hash',
            ),
          ),
        );
    case 'window_summary_hash':
      fields.add(
        ComputerUseTraceField(
          '期望摘要指纹',
          _string(
            check['expected_summary_hash'],
            'verification.check.expected_summary_hash',
          ),
        ),
      );
  }
  _addObservation(
    fields,
    _map(json['post_observation'], 'verification.post_observation'),
    '验证后观察',
  );
  final result = _map(json['result'], 'verification.result');
  final status = _enumName(result['status'], const {
    'passed',
    'failed',
  }, 'verification.result.status');
  fields.add(ComputerUseTraceField('验证结果', status));
  if (status == 'failed') {
    final evidence = _map(result['evidence'], 'verification.result.evidence');
    fields
      ..add(
        ComputerUseTraceField(
          '失败证据',
          _enumName(evidence['code'], const {
            'postcondition_not_met',
            'element_not_found',
            'unexpected_window',
            'driver_timeout',
            'other',
          }, 'verification.evidence.code'),
        ),
      )
      ..add(
        ComputerUseTraceField(
          '证据期望指纹',
          _nullableString(evidence['expected_hash']),
        ),
      )
      ..add(
        ComputerUseTraceField(
          '证据观察指纹',
          _nullableString(evidence['observed_summary_hash']),
        ),
      );
  }
}

String _eventTitle(String type) => switch (type) {
  'pre_dispatch_rejection' => '用户拒绝（派发前）',
  'observation' => '观察',
  'policy_decision' => '策略判定',
  'confirmation' => '用户确认',
  'action' => '动作',
  'receipt' => '驱动回执',
  'verification' => '动作后验证',
  'aborted' => '中止',
  _ => type,
};

String _timestamp(Object milliseconds) {
  // DateTime 的范围小于 u64；超出时保留原值，不能把无效时间改成别的日期。
  const maxDateTimeMilliseconds = 8640000000000000;
  final exact = milliseconds is BigInt
      ? milliseconds
      : BigInt.from(milliseconds as int);
  if (exact > BigInt.from(maxDateTimeMilliseconds)) {
    return '$milliseconds 毫秒（Unix 时间，UTC）';
  }
  return DateTime.fromMillisecondsSinceEpoch(
    exact.toInt(),
    isUtc: true,
  ).toIso8601String();
}

String _redacted(Object? raw, String what) {
  final json = _map(raw, what);
  if (_bool(json['redacted'], '$what.redacted') != true) {
    throw FormatException('$what 必须是显式脱敏值');
  }
  return '已脱敏';
}

Map<String, dynamic> _map(Object? raw, String what) {
  if (raw is Map<String, dynamic>) return raw;
  throw FormatException('$what 必须是对象');
}

List<Object?> _list(Object? raw, String what) {
  if (raw is List<Object?>) return raw;
  throw FormatException('$what 必须是数组');
}

String _string(Object? raw, String what) {
  if (raw is String && raw.isNotEmpty) return raw;
  throw FormatException('$what 必须是非空字符串');
}

String _nullableString(Object? raw) {
  if (raw == null) return '未记录';
  return _string(raw, 'optional string');
}

int _int(Object? raw, String what) {
  if (raw is int && raw >= 0) return raw;
  throw FormatException('$what 必须是非负整数');
}

Object _uint64(Object? raw, String what) {
  final maxUint64 = BigInt.parse('18446744073709551615');
  if (raw is BigInt && raw >= BigInt.zero && raw <= maxUint64) return raw;
  if (raw is int && raw >= 0 && (!kIsWeb || raw <= 9007199254740991)) {
    return raw;
  }
  throw FormatException('$what 必须是 u64 整数');
}

bool _bool(Object? raw, String what) {
  if (raw is bool) return raw;
  throw FormatException('$what 必须是布尔值');
}

String _enumName(Object? raw, Set<String> allowed, String what) {
  final value = _string(raw, what);
  if (!allowed.contains(value)) {
    throw FormatException('$what 含未知值: $value');
  }
  return value;
}

String _nullableEnumName(Object? raw, Set<String> allowed, String what) {
  if (raw == null) return '无';
  return _enumName(raw, allowed, what);
}
