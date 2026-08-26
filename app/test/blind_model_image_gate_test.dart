/// 贴了图，而这一轮的模型**已知**看不懂图 —— 拦在发送之前。
///
/// # 这一组盯住的四件事
///
/// 1. **回车那条路也要拦。** 按钮灰掉挡不住 Enter，而 Enter 是绝大多数人
///    真正在用的那个 —— 判据两处只落一处，漏的恰恰是主路径。
/// 2. **「不知道」不算「不行」。** 目录里查不到的新模型（`vision == null`）
///    要放行，与服务端 `ensure_can_see` 同一条约定。DeepSeek 2026-08-21
///    上线的 vision 模型在 models.dev 快照里一条都没有，按「不知道 = 不行」
///    处理会把唯一能解决问题的那个模型也挡住。
/// 3. **纯文本不受影响。** 拦的判据是「这一轮带着图」，不是「这个模型瞎」。
/// 4. **要给出路，而不只是拦住。** 一行提示 + 一个按钮。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/chat/widgets/message_composer.dart';
import 'package:cortex_app/models/attachment.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/attachment_controller.dart';
import 'package:cortex_app/state/model_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

class _MockConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: 'http://127.0.0.1:8080');
}

const _session = '01M0SESSIONAAAAAAAAAAAAAAA';

/// 最小合法 PNG：1×1 全透明。真字节而不是随手几个数 —— 上传那条路会
/// 按 mime 分流，喂一段假数据等于在测一条不存在的路。
final _png = Uint8List.fromList([
  0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, //
  0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
  0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
  0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
  0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41,
  0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
  0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
  0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
  0x42, 0x60, 0x82,
]);

/// 托盘里预先放好一张**已上传完成**的图。
///
/// # 为什么绕开真的上传路径
///
/// `addBytes` 会真的走一遍注册 blob 的异步链。在 widget test 里
/// `await` 它会**死锁**：那条链上的等待要靠 `tester.pump` 推进时钟，
/// 而 pump 在 await 返回之后才轮到 —— 实测就是这个，三条用例全部
/// 「did not complete」，跑满四分钟。
///
/// 而且上传链本来就有它自己的测试（`attachment_test.dart`）。这一组要
/// 钉的是**判据**，把两件事绑在一起只会让这里因为别处的改动变红。
class _SeededQueue extends AttachmentQueue {
  _SeededQueue(this.mime);

  final String mime;

  @override
  Map<String, List<PendingAttachment>> build() {
    super.build();
    return {
      _session: [
        PendingAttachment(
          id: 'p1',
          filename: mime.startsWith('image/') ? '截图.png' : '说明.txt',
          bytes: _png,
          mime: mime,
          status: UploadStatus.ready,
          attachment: Attachment(
            hash: 'a' * 64,
            mime: mime,
            filename: mime.startsWith('image/') ? '截图.png' : '说明.txt',
            sizeBytes: _png.length,
          ),
        ),
      ],
    };
  }
}

/// `vision` 三态直接覆写：判据本身才是这一组要测的东西，
/// 让它取决于 mock 目录里恰好有哪几个型号，等于测一件与判据无关的事。
///
/// `attach` 为 null = 托盘空着。
ProviderContainer _boot(bool? vision, {String? attach}) => ProviderContainer(
  overrides: [
    appConfigProvider.overrideWith(_MockConfig.new),
    cortexApiProvider.overrideWithValue(MockCortexApi(instant: true)),
    settingsReaderProvider.overrideWithValue(
      () async => const <String, String>{},
    ),
    settingsWriterProvider.overrideWithValue((_) async {}),
    selectedModelVisionProvider.overrideWithValue(vision),
    if (attach != null)
      attachmentQueueProvider.overrideWith(() => _SeededQueue(attach)),
  ],
);

/// 画出撰写框，并把 `onSend` 收到的东西记下来。
Future<List<(String, List<Attachment>)>> _pump(
  WidgetTester tester,
  ProviderContainer c,
) async {
  final sent = <(String, List<Attachment>)>[];
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: c,
      child: MaterialApp(
        home: Scaffold(
          body: MessageComposer(
            sessionId: _session,
            onSend: (t, a) => sent.add((t, a)),
            onStop: () {},
            streaming: false,
            ensureSession: () => _session,
          ),
        ),
      ),
    ),
  );
  await tester.pump(const Duration(milliseconds: 100));
  return sent;
}

/// 在输入框里敲字并按回车 —— **主路径**，绕开发送按钮。
Future<void> _typeAndEnter(WidgetTester tester, String text) async {
  await tester.enterText(find.byType(TextField).first, text);
  await tester.pump(const Duration(milliseconds: 50));
  await tester.sendKeyEvent(LogicalKeyboardKey.enter);
  for (var i = 0; i < 4; i++) {
    await tester.pump(const Duration(milliseconds: 50));
  }
}

void main() {
  testWidgets('模型已知看不懂图时，贴了图按回车发不出去，并给出一条出路', (tester) async {
    final c = _boot(false, attach: 'image/png');
    addTearDown(c.dispose);
    final sent = await _pump(tester, c);

    expect(
      find.textContaining('看不懂图'),
      findsOneWidget,
      reason:
          '要在**发送之前**说清楚。等服务端回一条红字的话，用户要等一次完整往返，'
          '而且看不出是自己选错了模型',
    );
    expect(
      find.text('换模型'),
      findsOneWidget,
      reason:
          '只说「不支持」等于把找模型这件事整个丢回给用户 —— '
          '而哪些模型看得懂图，是我们手上有目录、他没有的东西',
    );

    await _typeAndEnter(tester, '这是什么');
    expect(
      sent,
      isEmpty,
      reason:
          '⚠️ 按钮灰掉挡不住回车，而回车才是主路径。'
          '判据必须在 _submit 里也判一次，否则这一整道闸对绝大多数人等于不存在',
    );
  });

  testWidgets('能力未知（目录里查不到）时放行 —— 不知道不等于不行', (tester) async {
    final c = _boot(null, attach: 'image/png');
    addTearDown(c.dispose);
    final sent = await _pump(tester, c);

    expect(
      find.textContaining('看不懂图'),
      findsNothing,
      reason:
          'null 是「不知道」。按「不行」处理的话，刚发布、目录还没收录的 '
          'vision 模型会被自己人挡在门外 —— DeepSeek 的 vision 模型上线当天就是这样',
    );
    await _typeAndEnter(tester, '这是什么');
    expect(sent, hasLength(1), reason: '与服务端 ensure_can_see 同一条约定：只在显式声明不支持时拦');
  });

  testWidgets('瞎模型 + 纯文本照常发 —— 拦的是「这一轮带着图」，不是「这个模型瞎」', (tester) async {
    final c = _boot(false);
    addTearDown(c.dispose);
    final sent = await _pump(tester, c);

    expect(find.textContaining('看不懂图'), findsNothing, reason: '没贴图就不该有任何提示');
    await _typeAndEnter(tester, '你好');
    expect(
      sent,
      hasLength(1),
      reason: '判据写成「模型瞎就拦」的话，一个用 DeepSeek 的人连纯文本都发不出去',
    );
  });
}
