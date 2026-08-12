/// 换后端时，**旧后端**的待确认项不许落进新界面。
///
/// # 这条比「显示了一个错误」严重
///
/// 待确认项是「要不要让 agent 执行这条命令」的弹窗，带 token。
/// `ConfirmController` 换后端时会清空队列并重新拉一遍 —— 但它的
/// `_generation` **只在 `build()` 里 +1**，换后端时不 +1。
///
/// 于是一个在换后端之前发出、之后才回来的 `pendingConfirmations()`
/// 会通过 `_alive()` 的检查，把结果 merge 进已经清干净的 state。
/// 换的若是账号，用户会被问「要不要执行」——**而那是别人的命令**，
/// 他这边的 token 还对不上，点了也只会 404。
///
/// 与 chat / memory 那两处是同一个 bug 家族：换后端只清了**手上的**，
/// 没作废**在飞的**。
library;

import 'dart:async';

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/models/pending_confirmation.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/confirm_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

PendingConfirmation _pending(String token) => PendingConfirmation(
  token: token,
  sessionId: 's1',
  tool: 'shell',
  risk: 'execute',
  preview: 'rm -rf /',
  deadline: DateTime.utc(2030),
);

/// 旧后端：请求发出去了，换完后端才回来。
class _Slow extends MockCortexApi {
  final gate = Completer<List<PendingConfirmation>>();

  @override
  Future<List<PendingConfirmation>> pendingConfirmations({String? sessionId}) =>
      gate.future;
}

/// 新后端：什么也没在等。
class _Empty extends MockCortexApi {
  @override
  Future<List<PendingConfirmation>> pendingConfirmations({
    String? sessionId,
  }) async => const [];
}

void main() {
  test('换后端之前发出的待确认项，回来时不许落进新界面', () async {
    final old = _Slow();
    final fresh = _Empty();
    var swapped = false;

    final container = ProviderContainer(
      overrides: [
        cortexApiProvider.overrideWith((ref) => swapped ? fresh : old),
      ],
    );
    addTearDown(container.dispose);

    container.read(confirmControllerProvider); // 冷启动就拉一次
    await Future<void>.delayed(Duration.zero);

    swapped = true;
    container.invalidate(cortexApiProvider);
    container.read(cortexApiProvider);
    for (var i = 0; i < 4; i++) {
      await Future<void>.delayed(Duration.zero);
    }

    // 旧后端的响应现在才回来
    old.gate.complete([_pending('tok-from-old-backend')]);
    for (var i = 0; i < 4; i++) {
      await Future<void>.delayed(Duration.zero);
    }

    expect(
      container.read(confirmControllerProvider).pending,
      isEmpty,
      reason:
          '这是旧后端的确认项。换的若是账号，用户会被问「要不要执行」—— '
          '而那是别人的命令，token 在他这边根本对不上',
    );
  });
}
