import 'dart:async';
import 'dart:typed_data';

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/import/import_source.dart';
import 'package:cortex_app/models/import_plan.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/import_controller.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

import 'support/replay_api.dart';

ImportEstimate _estimate({int pairs = 10}) => ImportEstimate(
  platform: 'Claude',
  conversations: 2,
  messages: pairs * 2,
  pairs: pairs,
  tokens: 1234,
  unpaired: 0,
  minutes: 1,
);

/// Records what was asked of it and replays a scripted run.
class _ImportApi extends ReplayApi {
  _ImportApi({this.events = const [], this.failPreview = false})
    : super(episodeCount: 0);

  final List<ImportEvent> events;
  final bool failPreview;

  int prepareCalls = 0;
  int previewCalls = 0;
  int runCalls = 0;
  int? lastMax;

  @override
  Future<ImportTarget> prepareImport(ImportSource source) async {
    prepareCalls++;
    return const ImportTargetPath('D:/x/conversations.json');
  }

  @override
  Future<ImportEstimate> importPreview(
    ImportTarget target, {
    int? maxConversations,
  }) async {
    previewCalls++;
    lastMax = maxConversations;
    if (failPreview) {
      throw const CortexApiException('结构认不出来：见到的键是 foo, bar');
    }
    return _estimate(pairs: maxConversations ?? 10);
  }

  @override
  Stream<ImportEvent> runImport(
    ImportTarget target, {
    int? maxConversations,
  }) async* {
    runCalls++;
    for (final e in events) {
      yield e;
    }
  }
}

/// 替代平台文件选择框。测试里弹不出对话框，但除此之外走的是完全一样的路径。
Future<ImportSource?> _picked() async => const ImportPath(
  path: 'D:/x/conversations.json',
  filename: 'conversations.json',
  sizeBytes: 97218704,
);

ProviderContainer _container(_ImportApi api) {
  final c = ProviderContainer(
    overrides: [cortexApiProvider.overrideWithValue(api)],
  );
  addTearDown(c.dispose);
  return c;
}

void main() {
  group('导入控制器', () {
    /// **没看过账就不能开始。**
    ///
    /// 这是这块唯一「错了会花钱」的地方：一次导入是几千回 LLM 调用，
    /// 而记忆 append-only，点错了撤不掉。所以 start() 在 estimated
    /// 之外的任何状态都必须是空操作，而不是「大概也能跑」。
    test('账没摊开之前，start 什么都不做', () async {
      final api = _ImportApi();
      final c = _container(api);
      final ctrl = c.read(importControllerProvider.notifier);

      ctrl.start();
      expect(api.runCalls, 0, reason: 'idle 状态下 start 必须是空操作');
      expect(c.read(importControllerProvider).phase, ImportPhase.idle);
    });

    test('跑完之后再点 start 也不会重跑', () async {
      final api = _ImportApi(
        events: const [ImportDoneEvent(pairsDone: 3, skipped: 0, failures: 0)],
      );
      final c = _container(api);
      final ctrl = c.read(importControllerProvider.notifier);

      await ctrl.pickAndPreview(pick: _picked);
      ctrl.start();
      await Future<void>.delayed(Duration.zero);
      expect(c.read(importControllerProvider).phase, ImportPhase.finished);

      ctrl.start();
      // 必须等一个微任务再断言：`runImport` 是 async*，函数体（也就是
      // runCalls++）要到订阅之后的下一个微任务才跑。少了这一句，
      // 这条用例在闸门被拆掉时**依然会绿** —— 实测过
      await Future<void>.delayed(Duration.zero);
      expect(api.runCalls, 1, reason: 'finished 之后再点 start 必须是空操作');
    });

    /// 事件按顺序落到状态上，done 之后停在 finished。
    test('started → progress → done 逐个落到状态上', () async {
      final api = _ImportApi(
        events: [
          ImportStartedEvent(_estimate(pairs: 7)),
          const ImportProgressEvent(
            conversationsDone: 1,
            conversationsTotal: 2,
            pairsDone: 4,
            skipped: 2,
            failures: 0,
          ),
          const ImportDoneEvent(pairsDone: 7, skipped: 2, failures: 1),
        ],
      );
      final c = _container(api);
      final ctrl = c.read(importControllerProvider.notifier);
      await ctrl.pickAndPreview(pick: _picked);
      expect(c.read(importControllerProvider).phase, ImportPhase.estimated);

      ctrl.start();
      await Future<void>.delayed(Duration.zero);

      final s = c.read(importControllerProvider);
      expect(s.phase, ImportPhase.finished);
      expect(
        s.estimate?.pairs,
        7,
        reason: 'started 里的账是权威的 —— preview 那份可能已经过时',
      );
      expect(s.progress?.skipped, 2);
      expect(s.done?.failures, 1);
      expect(s.error, isNull);
    });

    /// **流没给 done 就断了，不能显示成成功。**
    ///
    /// 十几分钟的导入中途掉线是真会发生的。停在 finished 而不给任何说明，
    /// 用户会以为跑完了 —— 而实际上服务端那边还在跑。
    test('连接中途断掉时要说清楚，而不是静静地变成完成', () async {
      final api = _ImportApi(events: const []); // 一个事件都不发就结束
      final c = _container(api);
      final ctrl = c.read(importControllerProvider.notifier);
      await ctrl.pickAndPreview(pick: _picked);
      expect(c.read(importControllerProvider).phase, ImportPhase.estimated);

      ctrl.start();
      await Future<void>.delayed(Duration.zero);

      final s = c.read(importControllerProvider);
      expect(s.phase, ImportPhase.finished);
      expect(s.done, isNull);
      expect(s.error, isNotNull, reason: '没有 done 就不能表现成成功');
      expect(s.error, contains('重新导入'), reason: '要告诉用户重跑是安全的');
    });

    /// 改「只导最近 N 段」只重算账，不重新上传。
    ///
    /// Web 端上传的是 97 MB —— 每调一次范围就重传一次是不可接受的。
    test('改导入范围复用已经交出去的文件', () async {
      final api = _ImportApi();
      final c = _container(api);
      final ctrl = c.read(importControllerProvider.notifier);
      await ctrl.pickAndPreview(pick: _picked);
      final previewsAfterPick = api.previewCalls;

      await ctrl.setMaxConversations(5);

      expect(api.prepareCalls, 1, reason: '文件只交一次（Web 端那是 97MB）');
      expect(api.previewCalls, previewsAfterPick + 1);
      expect(api.lastMax, 5);
      expect(c.read(importControllerProvider).estimate?.pairs, 5);
    });

    /// 解析器的原话要原样带到界面上。
    ///
    /// 它被写成「响亮失败并列出实际见到的键」，改写成「导入失败」
    /// 就把格式漂移唯一的线索扔了。
    test('服务端的错误原样保留', () async {
      final api = _ImportApi(failPreview: true);
      final c = _container(api);
      final ctrl = c.read(importControllerProvider.notifier);
      await ctrl.pickAndPreview(pick: _picked);

      final s = c.read(importControllerProvider);
      expect(s.error, contains('foo, bar'), reason: '解析器列出的键是唯一的线索');
      expect(
        s.phase,
        ImportPhase.idle,
        reason: '看不到账就绝不能停在一个能按「开始」的状态',
      );
    });
  });

  group('挑文件的那一半', () {
    /// 桌面端拿到的是路径，Web 端拿到的是字节 —— 两者进请求体的形状不同，
    /// 而这个区分决定了 97 MB 会不会过网络。
    test('两种来源各自转成正确的请求体', () {
      const path = ImportTargetPath('D:/x.json');
      expect(path.locator, {'path': 'D:/x.json'});

      const handle = ImportTargetHandle('01ABC', expiresInSecs: 3600);
      expect(handle.locator, {'handle': '01ABC'});
    });

    test('两种 ImportSource 都带着文件名与大小', () {
      const p = ImportPath(path: 'a', filename: 'c.json', sizeBytes: 97);
      expect(p.filename, 'c.json');
      expect(p.sizeBytes, 97);

      final b = ImportBytes(
        bytes: Uint8List(3),
        filename: 'c.json',
        sizeBytes: 3,
      );
      expect(b.bytes.length, 3);
    });
  });
}
