/// 远程接入那个开关：文案说没说破、拨动通不通、答不出时画不画。
///
/// # 为什么文案要写测试
///
/// 这是安全不变量 4：接入面里 `POST /chat` 与 `POST /confirmations` 并存，
/// 接进来的一方能发起一轮**并自己批准**工具确认。所以打开它等于同意远程侧
/// 可经模型在这台机器上执行命令与读写文件。
///
/// 用户对一个开关的全部理解就是它旁边那段话。写软成「允许远程查看」不会有
/// 任何报错 —— 而他按下的就是一个自己没读懂的开关。`cortex-local` 那侧的
/// `--allow-remote-attach` 说明由一条 Rust 测试守着同一组词。
library;

import 'package:cortex_app/api/api_exception.dart';
import 'package:cortex_app/api/cortex_api.dart';
import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/features/settings/pages/machines_page.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/remote_attach_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// 一个不认识这条路的后端（Web 端、纯 cortexd、比它旧的本地 agent）。
class _NoAttachApi extends MockCortexApi {
  @override
  Future<bool> localAttach() =>
      Future.error(const CortexApiException('这个后端没有远程接入开关。', statusCode: 404));

  @override
  Future<bool> setLocalAttach(bool enabled) => localAttach();
}

/// 拨动会失败的后端 —— 用来验「失败不许被吞掉」。
class _FailingApi extends MockCortexApi {
  @override
  Future<bool> localAttach() async => false;

  @override
  Future<bool> setLocalAttach(bool enabled) =>
      Future.error(const CortexApiException('本机 agent 没应答。'));
}

/// ⚠️ **名册那条流必须换掉。** 真的那条是 10 秒一轮的死循环，
/// widget 测试收尾时会因为「还有 Timer 挂着」直接失败 ——
/// 而这几条要看的是那张卡片，与名册里有几台无关。
Widget _page(CortexApi api) => ProviderScope(
  overrides: [
    cortexApiProvider.overrideWithValue(api),
    machinesProvider.overrideWith((ref) => Stream.value(const [])),
  ],
  child: const MaterialApp(home: Scaffold(body: MachinesPage())),
);

void main() {
  test('那段说明必须说破它交出了什么', () {
    // 与 Rust 那侧 `开放接入的说明必须说破它交出了什么` 同一组词。
    // 两处各自守自己那一段：一个是命令行的 --help，一个是界面上的开关
    for (final must in ['执行', '确认', '默认关闭']) {
      expect(
        kRemoteAttachExplainer,
        contains(must),
        reason:
            '说明里没有「$must」—— 用户对一个开关的全部理解就是它旁边这段话，'
            '写软成「允许远程查看」不会有任何报错',
      );
    }
    expect(
      kRemoteAttachOffNote,
      contains('断开'),
      reason: '关掉是当场断开已有连接的，不说的话用户会以为「下次才生效」',
    );
  });

  testWidgets('答不出这条路的后端，整张卡片不画', (tester) async {
    await tester.pumpWidget(_page(_NoAttachApi()));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(
      find.text('这台机器 · 远程接入'),
      findsNothing,
      reason:
          '画一个永远关着的开关比不画更糟 —— 用户会以为自己关着，'
          '而这个后端根本答不出它开没开',
    );
  });

  testWidgets('拨动会把落定之后的状态显示出来', (tester) async {
    // 夹具里是**开着**的（那一档才需要盯着看）
    await tester.pumpWidget(_page(MockCortexApi()));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(find.text('开着 —— 其他设备可以接进来'), findsOneWidget);
    expect(
      find.text(kRemoteAttachOffNote),
      findsOneWidget,
      reason: '开着的时候才需要告诉他「关掉会当场断开」',
    );

    await tester.tap(find.byType(Switch));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(find.text('关着 —— 只有这台机器上的 Cortex 用得到它'), findsOneWidget);
    expect(
      find.text(kRemoteAttachOffNote),
      findsNothing,
      reason: '关着的时候那句话是废话',
    );
  });

  testWidgets('拨不动的时候必须说出来，不许静默留在原状', (tester) async {
    await tester.pumpWidget(_page(_FailingApi()));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(find.text('关着 —— 只有这台机器上的 Cortex 用得到它'), findsOneWidget);
    await tester.tap(find.byType(Switch));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));

    expect(
      find.textContaining('打开远程接入失败'),
      findsOneWidget,
      reason:
          '静默失败在这个开关上格外贵：反过来那一次（关不掉）会让用户'
          '以为自己关了，而云端接得进来',
    );
    expect(
      find.text('关着 —— 只有这台机器上的 Cortex 用得到它'),
      findsOneWidget,
      reason: '失败之后开关必须留在原处 —— 乐观地拨过去就是在撒谎',
    );
  });
}
