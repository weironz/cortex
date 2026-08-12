/// 冷启动时「记忆检索失败」，点一次刷新就好了。
///
/// # 这个 bug 的完整链条
///
/// 1. 应用刚起来，本地 agent 还没就绪 → `cortexApiProvider` 指向**远端**
/// 2. 记忆面板打开，`/memory/search` 发了出去
/// 3. 一两秒后 agent 起好 → provider 重建 → 旧 `HttpCortexApi` 被
///    `dispose`，而它的 dispose 是 `_client.close()` ——
///    **正在飞的那个请求当场被掐断**
/// 4. 抛出 `Connection closed before full header was received`
/// 5. 那个异常在「换后端就清空」之后才落地，于是把 `error` 又写了回去
///
/// 结果：界面停在「检索失败」，而用户什么也没做错。**每次冷启动都命中**。
///
/// 修法是把在飞的请求作废（序号 +1）并自动重来一次。这两条分别盯住
/// 「尸体不许覆盖界面」与「不该让用户去点刷新」。
library;

import 'dart:async';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/models/memory_search_result.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/memory_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// 一个「请求发出去之后就被掐断」的替身 —— 复现 `_client.close()`。
class _Aborting extends MockCortexApi {
  final completer = Completer<MemorySearchResult>();

  @override
  Future<MemorySearchResult> searchMemory(
    String query, {
    int limit = 30,
    DateTime? asOf,
  }) => completer.future;
}

/// 换后端之后那个「好的」替身。
class _Working extends MockCortexApi {
  int calls = 0;

  @override
  Future<MemorySearchResult> searchMemory(
    String query, {
    int limit = 30,
    DateTime? asOf,
  }) async {
    calls++;
    return const MemorySearchResult(facts: [], channels: {});
  }
}

void main() {
  late _Aborting first;
  late _Working second;
  late bool swapped;

  ProviderContainer boot() {
    first = _Aborting();
    second = _Working();
    swapped = false;
    return ProviderContainer(
      overrides: [
        cortexApiProvider.overrideWith(
          (ref) => swapped ? second : first,
        ),
      ],
    );
  }

  test('换后端时，被掐断的那个请求不许把「检索失败」写回界面', () async {
    final container = boot();
    addTearDown(container.dispose);
    final notifier = container.read(memoryControllerProvider.notifier);

    unawaited(notifier.search()); // 打到「远端」，还没回来
    await Future<void>.delayed(Duration.zero);
    expect(container.read(memoryControllerProvider).loading, isTrue);

    // agent 就绪 → provider 重建
    swapped = true;
    container.invalidate(cortexApiProvider);
    container.read(cortexApiProvider);
    await Future<void>.delayed(Duration.zero);

    // 旧客户端被 dispose，在飞的请求当场断掉
    first.completer.completeError(
      CortexApiException('Connection closed before full header was received'),
    );
    await Future<void>.delayed(Duration.zero);
    await Future<void>.delayed(Duration.zero);

    expect(
      container.read(memoryControllerProvider).error,
      isNull,
      reason: '那个请求是被我们自己掐断的，不是检索真的失败了。'
          '把它的异常写进界面，用户看到的是一个他无法理解、也无法处理的错误 —— '
          '而唯一的出路是手点刷新',
    );
  });

  test('换后端之后自动重来一次，不该让用户去点刷新', () async {
    final container = boot();
    addTearDown(container.dispose);
    final notifier = container.read(memoryControllerProvider.notifier);

    unawaited(notifier.search());
    await Future<void>.delayed(Duration.zero);

    swapped = true;
    container.invalidate(cortexApiProvider);
    container.read(cortexApiProvider);
    await Future<void>.delayed(Duration.zero);
    await Future<void>.delayed(Duration.zero);

    expect(
      second.calls,
      greaterThan(0),
      reason: '换到新后端之后要自己重跑一次。不重跑的话面板是空的，'
          '而用户不知道那是「没有结果」还是「还没查」',
    );
  });

  test('没搜过就不要自作主张地去搜', () async {
    final container = boot();
    addTearDown(container.dispose);
    container.read(memoryControllerProvider); // 只是建起来，没搜

    swapped = true;
    container.invalidate(cortexApiProvider);
    container.read(cortexApiProvider);
    await Future<void>.delayed(Duration.zero);
    await Future<void>.delayed(Duration.zero);

    expect(
      second.calls,
      0,
      reason: '面板可能根本没打开。为一个没人在看的面板发请求，'
          '在按量计费的部署上就是白花钱',
    );
  });
}
