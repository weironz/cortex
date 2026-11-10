import 'dart:async';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/core/computer_input_recovery_api.dart';
import 'package:cortex_app/core/permission_mode.dart';
import 'package:cortex_app/features/settings/pages/computer_input_recovery_section.dart';
import 'package:cortex_app/features/settings/pages/computer_use_section.dart';
import 'package:cortex_app/models/health_status.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/auth_controller.dart';
import 'package:cortex_app/state/chat_controller.dart';
import 'package:cortex_app/state/chat_state.dart';
import 'package:cortex_app/state/model_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

const _needsStop = ComputerInputRecoveryState(
  phase: ComputerInputRecoveryPhase.needsStop,
  recoveryId: 'recovery-1',
  operationId: 'ab12',
  epoch: 7,
);
const _needsReview = ComputerInputRecoveryState(
  phase: ComputerInputRecoveryPhase.needsReview,
  recoveryId: 'recovery-1',
  operationId: 'ab12',
  epoch: 7,
);
const _working = ComputerInputRecoveryState(
  phase: ComputerInputRecoveryPhase.working,
  recoveryId: 'recovery-1',
  operationId: 'ab12',
  epoch: 7,
);
const _complete = ComputerInputRecoveryState(
  phase: ComputerInputRecoveryPhase.complete,
  recoveryId: 'recovery-1',
  operationId: 'ab12',
  epoch: 7,
);

class _RecoveryApi extends MockCortexApi implements ComputerInputRecoveryApi {
  _RecoveryApi() : super(instant: true);
  ComputerInputRecoveryState current = _needsStop;
  ComputerInputRecoveryState retireResult = _needsReview;
  ComputerInputRecoveryState acknowledgeResult = _complete;
  ComputerInputRecoveryState? afterRetire;
  ComputerInputRecoveryState? afterAcknowledge;
  Object? retireError;
  Future<ComputerInputRecoveryState> Function()? getResult;
  final gets = <String>[];
  final retires = <(String, String)>[];
  final acknowledgements = <(String, String)>[];
  int aborts = 0;

  void track(Future<void>? abort) {
    if (abort != null) {
      unawaited(
        abort.then((_) {
          aborts++;
        }),
      );
    }
  }

  @override
  Future<ComputerInputRecoveryState> inputRecovery(
    String sessionId, {
    Future<void>? abortTrigger,
  }) {
    gets.add(sessionId);
    track(abortTrigger);
    return getResult?.call() ?? Future.value(current);
  }

  @override
  Future<ComputerInputRecoveryState> retireInputRecovery(
    String sessionId, {
    required String recoveryId,
    Future<void>? abortTrigger,
  }) async {
    retires.add((sessionId, recoveryId));
    track(abortTrigger);
    final error = retireError;
    if (error != null) throw error;
    current = afterRetire ?? retireResult;
    return retireResult;
  }

  @override
  Future<ComputerInputRecoveryState> acknowledgeInputRecovery(
    String sessionId, {
    required String recoveryId,
    Future<void>? abortTrigger,
  }) async {
    acknowledgements.add((sessionId, recoveryId));
    track(abortTrigger);
    current = afterAcknowledge ?? acknowledgeResult;
    return acknowledgeResult;
  }
}

class _Config extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:9');
}

class _Account extends AuthController {
  @override
  AuthState build() =>
      const AuthState(phase: AuthPhase.ready, token: 'test-account-a');
  void change() =>
      state = const AuthState(phase: AuthPhase.ready, token: 'test-account-b');
}

class _Chat extends ChatController {
  _Chat(this.session);
  final String? session;
  @override
  ChatState build() => ChatState(activeSessionId: session);
  void change(String value) => state = state.copyWith(activeSessionId: value);
}

class _Bypass extends PermissionModeNotifier {
  @override
  PermissionMode build() => PermissionMode.bypass;
}

ProviderContainer _container(
  CortexApi api, {
  String? session = 'existing-session',
  ComputerWriteReadiness? readiness =
      ComputerWriteReadiness.authorityUnresolved,
}) => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_Config.new),
    cortexApiProvider.overrideWithValue(api),
    authControllerProvider.overrideWith(_Account.new),
    chatControllerProvider.overrideWith(() => _Chat(session)),
    permissionModeProvider.overrideWith(_Bypass.new),
    healthProvider.overrideWith(
      (_) async => HealthStatus(
        status: 'ok',
        version: 'test',
        database: 'unknown',
        role: HealthStatus.roleLocalAgent,
        computerUse: true,
        computerCapabilities: ComputerCapabilities(
          observe: true,
          act: true,
          writeReadiness: readiness,
        ),
      ),
    ),
    settingsReaderProvider.overrideWithValue(
      () async => const <String, String>{},
    ),
    settingsWriterProvider.overrideWithValue((_) async {}),
    selectedModelVisionProvider.overrideWithValue(true),
  ],
);

Future<void> _mount(
  WidgetTester tester,
  ProviderContainer container, {
  bool fullSettings = false,
}) async {
  addTearDown(() async {
    await tester.pumpWidget(const SizedBox.shrink());
    container.dispose();
  });
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: container,
      child: MaterialApp(
        home: Scaffold(
          body: SingleChildScrollView(
            child: fullSettings
                ? const ComputerUseSection()
                : const ComputerInputRecoverySection(),
          ),
        ),
      ),
    ),
  );
  await tester.pumpAndSettle();
}

Future<void> _tap(WidgetTester tester, String name) async {
  final button = find.byKey(ValueKey('computer_input_recovery:$name'));
  await tester.ensureVisible(button);
  await tester.tap(button);
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('核对提交返回 working 时也只 GET，不重复 acknowledge', (tester) async {
    final api = _RecoveryApi()
      ..acknowledgeResult = _working
      ..afterAcknowledge = _complete;
    await _mount(tester, _container(api));
    await _tap(tester, 'retire');
    await _tap(tester, 'confirm-stop');
    await _tap(tester, 'review');
    await _tap(tester, 'confirm-review');
    await tester.pump(const Duration(milliseconds: 600));
    await tester.pumpAndSettle();
    expect(api.retires, hasLength(1));
    expect(api.acknowledgements, hasLength(1));
    expect(api.gets.length, greaterThan(1));
    expect(find.textContaining('后续输入已恢复'), findsOneWidget);
  });

  testWidgets('原 operation 或 epoch 改变时不能消费旧确认继续恢复', (tester) async {
    final api = _RecoveryApi()
      ..retireResult = const ComputerInputRecoveryState(
        phase: ComputerInputRecoveryPhase.needsReview,
        recoveryId: 'recovery-1',
        operationId: 'cd34',
        epoch: 8,
      );
    await _mount(tester, _container(api));
    await _tap(tester, 'retire');
    await _tap(tester, 'confirm-stop');
    expect(find.textContaining('原操作或输入代际已变化'), findsOneWidget);
    expect(
      find.byKey(const ValueKey('computer_input_recovery:review')),
      findsNothing,
    );
    expect(api.acknowledgements, isEmpty);
  });

  testWidgets('停止步骤不能用 complete 跳过独立的外部结果核对', (tester) async {
    final api = _RecoveryApi()..retireResult = _complete;
    await _mount(tester, _container(api));
    await _tap(tester, 'retire');
    await _tap(tester, 'confirm-stop');
    expect(api.retires, hasLength(1));
    expect(api.acknowledgements, isEmpty);
    expect(find.textContaining('停止步骤不能代替外部结果核对'), findsOneWidget);
    expect(find.textContaining('后续输入已恢复'), findsNothing);
  });

  testWidgets('完全放行也必须先确认停止，再独立确认外部结果', (tester) async {
    final api = _RecoveryApi();
    final c = _container(api);
    await _mount(tester, c);
    expect(c.read(permissionModeProvider), PermissionMode.bypass);
    expect(find.text('原操作：ab12'), findsOneWidget);
    expect(find.text('输入代际：7'), findsOneWidget);
    await _tap(tester, 'retire');
    expect(find.text('停止原执行器？'), findsOneWidget);
    expect(api.retires, isEmpty, reason: '打开对话框不代表用户同意');
    await _tap(tester, 'confirm-stop');
    expect(api.retires, [('existing-session', 'recovery-1')]);
    expect(api.acknowledgements, isEmpty, reason: '停止不等于核对了外部结果');
    expect(find.textContaining('后续输入已恢复'), findsNothing);
    await _tap(tester, 'review');
    expect(find.text('已核对外部结果？'), findsOneWidget);
    expect(api.acknowledgements, isEmpty);
    await _tap(tester, 'confirm-review');
    expect(api.acknowledgements, [('existing-session', 'recovery-1')]);
    expect(find.textContaining('后续输入已恢复'), findsOneWidget);
    expect(api.gets, everyElement('existing-session'));
  });

  testWidgets('停止失败返回 needs_stop 时展示原因，重试仍需两步独立确认', (tester) async {
    final api = _RecoveryApi()
      ..retireResult = const ComputerInputRecoveryState(
        phase: ComputerInputRecoveryPhase.needsStop,
        recoveryId: 'recovery-1',
        operationId: 'ab12',
        epoch: 7,
        message: '无法确认原执行器已停止，请稍后重新确认停止。',
      );
    await _mount(tester, _container(api));
    await _tap(tester, 'retire');
    await _tap(tester, 'confirm-stop');
    expect(api.retires, hasLength(1));
    expect(find.text('无法确认原执行器已停止，请稍后重新确认停止。'), findsOneWidget);
    expect(
      find.byKey(const ValueKey('computer_input_recovery:retire')),
      findsOneWidget,
    );
    expect(
      find.byKey(const ValueKey('computer_input_recovery:review')),
      findsNothing,
    );
    expect(find.textContaining('后续输入已恢复'), findsNothing);
    expect(api.acknowledgements, isEmpty);

    api.retireResult = _needsReview;
    await _tap(tester, 'retire');
    expect(find.text('停止原执行器？'), findsOneWidget);
    expect(api.retires, hasLength(1), reason: '上次的停止确认不能授权本次重试');
    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    expect(api.retires, hasLength(1), reason: '取消重试必须保留暂停');
    await _tap(tester, 'retire');
    await _tap(tester, 'confirm-stop');
    expect(api.retires, hasLength(2));
    expect(api.acknowledgements, isEmpty, reason: '停止重试成功仍不等于核对外部结果');
    expect(find.textContaining('后续输入已恢复'), findsNothing);
    await _tap(tester, 'review');
    expect(find.text('已核对外部结果？'), findsOneWidget);
    expect(api.acknowledgements, isEmpty);
    await _tap(tester, 'confirm-review');
    expect(api.acknowledgements, [('existing-session', 'recovery-1')]);
    expect(find.textContaining('后续输入已恢复'), findsOneWidget);
  });

  testWidgets('两个确认框的取消都保留暂停，不提交对应 POST', (tester) async {
    final api = _RecoveryApi();
    await _mount(tester, _container(api));
    await _tap(tester, 'retire');
    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    expect(api.retires, isEmpty);
    expect(
      find.byKey(const ValueKey('computer_input_recovery:retire')),
      findsOneWidget,
    );
    await _tap(tester, 'retire');
    await _tap(tester, 'confirm-stop');
    await _tap(tester, 'review');
    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();
    expect(api.retires, hasLength(1));
    expect(api.acknowledgements, isEmpty);
    expect(
      find.byKey(const ValueKey('computer_input_recovery:review')),
      findsOneWidget,
    );
    expect(find.textContaining('后续输入已恢复'), findsNothing);
  });

  testWidgets('working 仅轮询 GET，到 needs_review 后等待第二次人工确认', (tester) async {
    final api = _RecoveryApi()
      ..retireResult = _working
      ..afterRetire = _needsReview;
    await _mount(tester, _container(api));
    await _tap(tester, 'retire');
    await _tap(tester, 'confirm-stop');
    await tester.pump(const Duration(milliseconds: 600));
    await tester.pumpAndSettle();
    expect(api.retires, hasLength(1), reason: '轮询不能重发 retire');
    expect(api.gets.length, greaterThan(1));
    expect(api.acknowledgements, isEmpty);
    expect(
      find.byKey(const ValueKey('computer_input_recovery:review')),
      findsOneWidget,
    );
    expect(find.textContaining('后续输入已恢复'), findsNothing);
  });

  testWidgets('working 的30秒总预算到期即停止，不能显示完成', (tester) async {
    final api = _RecoveryApi()..current = _working;
    await _mount(tester, _container(api));
    await tester.pump(const Duration(seconds: 31));
    await tester.pumpAndSettle();
    expect(find.textContaining('等待已超过30秒'), findsOneWidget);
    final calls = api.gets.length;
    expect(calls, lessThanOrEqualTo(61));
    await tester.pump(const Duration(seconds: 10));
    expect(api.gets.length, calls);
    expect(api.aborts, greaterThan(0));
    expect(api.retires, isEmpty);
    expect(api.acknowledgements, isEmpty);
    expect(find.textContaining('后续输入已恢复'), findsNothing);
  });

  testWidgets('销毁取消 pending GET，迟到 complete 不更新旧界面', (tester) async {
    final late = Completer<ComputerInputRecoveryState>();
    final api = _RecoveryApi()..getResult = (() => late.future);
    await _mount(tester, _container(api));
    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump();
    expect(api.aborts, 1);
    late.complete(_complete);
    await tester.pump(const Duration(seconds: 2));
    expect(api.gets, hasLength(1));
    expect(tester.takeException(), isNull);
  });

  testWidgets('账号改变废弃旧请求结果，会话改变关闭旧确认框', (tester) async {
    final late = Completer<ComputerInputRecoveryState>();
    var calls = 0;
    final api = _RecoveryApi()
      ..getResult = (() =>
          calls++ == 0 ? late.future : Future.value(_needsStop));
    final c = _container(api);
    await _mount(tester, c);
    (c.read(authControllerProvider.notifier) as _Account).change();
    await tester.pumpAndSettle();
    late.complete(_complete);
    await tester.pumpAndSettle();
    expect(api.gets, hasLength(2), reason: '新账号必须重新 GET 证明当前会话');
    expect(find.textContaining('后续输入已恢复'), findsNothing);
    await _tap(tester, 'retire');
    (c.read(chatControllerProvider.notifier) as _Chat).change(
      'another-existing-session',
    );
    await tester.pumpAndSettle();
    expect(find.byType(AlertDialog), findsNothing);
    expect(api.retires, isEmpty, reason: '旧会话上的人工确认不能传给新会话');
    expect(api.gets.last, 'another-existing-session');
  });

  testWidgets('缺当前会话不自动创建或查询恢复', (tester) async {
    final api = _RecoveryApi();
    final c = _container(api, session: null);
    await _mount(tester, c);
    expect(find.textContaining('请先打开对应账号下已有的会话'), findsOneWidget);
    expect(c.read(chatControllerProvider).activeSessionId, isNull);
    expect(api.gets, isEmpty);
  });

  testWidgets('旧 API 不提供恢复动作', (tester) async {
    final unsupported = _container(MockCortexApi(instant: true));
    await _mount(tester, unsupported);
    expect(find.textContaining('当前连接不支持输入恢复'), findsOneWidget);
    expect(
      find.byKey(const ValueKey('computer_input_recovery:retire')),
      findsNothing,
    );
  });

  testWidgets('404 与失败阶段保持暂停，失败的 acknowledge 不显示完成', (tester) async {
    final api = _RecoveryApi()
      ..getResult = (() =>
          Future.error(const CortexApiException('不存在', statusCode: 404)));
    await _mount(tester, _container(api));
    expect(find.textContaining('当前会话的输入恢复不可用'), findsOneWidget);
    expect(find.textContaining('后续输入已恢复'), findsNothing);
    api.getResult = null;
    await _tap(tester, 'refresh');
    await _tap(tester, 'retire');
    await _tap(tester, 'confirm-stop');
    api.acknowledgeResult = const ComputerInputRecoveryState(
      phase: ComputerInputRecoveryPhase.failed,
      message: '原执行器排空核验失败',
    );
    await _tap(tester, 'review');
    await _tap(tester, 'confirm-review');
    expect(find.textContaining('恢复未完成，输入保持暂停'), findsOneWidget);
    expect(find.text('原执行器排空核验失败'), findsOneWidget);
    expect(find.textContaining('后续输入已恢复'), findsNothing);
  });

  testWidgets('retire 请求失败不自动重发，也不开放核对恢复按钮', (tester) async {
    final api = _RecoveryApi()
      ..retireError = const CortexApiException('原执行器尚未停止', statusCode: 503);
    await _mount(tester, _container(api));
    await _tap(tester, 'retire');
    await _tap(tester, 'confirm-stop');
    await tester.pump(const Duration(seconds: 2));
    expect(api.retires, hasLength(1));
    expect(api.acknowledgements, isEmpty);
    expect(find.textContaining('输入保持暂停'), findsWidgets);
    expect(
      find.byKey(const ValueKey('computer_input_recovery:review')),
      findsNothing,
    );
    expect(find.textContaining('后续输入已恢复'), findsNothing);
  });

  testWidgets('设置只在 authority_unresolved 展示恢复入口', (tester) async {
    final api = _RecoveryApi();
    await _mount(
      tester,
      _container(api, readiness: ComputerWriteReadiness.ready),
      fullSettings: true,
    );
    expect(find.text('输入恢复'), findsNothing);
    expect(api.gets, isEmpty);
  });

  testWidgets('设置在禁止写入时仍能展示用户恢复入口', (tester) async {
    final api = _RecoveryApi();
    final c = _container(api);
    await _mount(tester, c, fullSettings: true);
    expect(c.read(computerActAvailableProvider), isFalse);
    expect(find.text('输入恢复'), findsOneWidget);
    expect(
      find.byKey(const ValueKey('computer_input_recovery:retire')),
      findsOneWidget,
    );
  });
}
