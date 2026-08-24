/// 资料库这一页 —— 空态说得出怎么放东西进来、三页签真的重取、
/// 「提不出正文」照实说。
///
/// # 这一组在盯的三件事
///
/// 1. **空态要给入口。** 一个只说「还是空的」的空态是死路 —— 用户不知道
///    材料怎么进去（它不是从这一页上传的，是从对话里拖进去的）。
/// 2. **换页签是重取不是过滤手上这些。** 只过滤这一页的话，「文件」页会
///    少掉所有还没翻到的，而它看起来是完整的（与图片页同一条）。
/// 3. **pdf/docx 那句「提不出正文」必须出现。** 不说的话用户会以为它们
///    也能被检索到，然后因为模型查不到而认定资料库坏了 ——
///    CLAUDE.md 约束 2 的界面版本。
library;

import 'package:cortex_app/api/mock_cortex_api.dart';
import 'package:cortex_app/core/app_config.dart';
import 'package:cortex_app/features/library/library_page.dart';
import 'package:cortex_app/models/library_item.dart';
import 'package:cortex_app/state/app_providers.dart';
import 'package:cortex_app/state/library_controller.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

/// 只替换 `library` 一个方法的替身 —— 其余照 mock 走。
class _LibApi extends MockCortexApi {
  _LibApi({this.items = const []}) : super(instant: true);

  final List<LibraryItem> items;

  /// 每次请求带的 (folder, tab) —— 用来验「换页签是重取」。
  final List<({String? folder, String? tab})> calls = [];

  @override
  Future<LibraryPage> library({
    int limit = 60,
    String? before,
    String? folder,
    String? tab,
  }) async {
    calls.add((folder: folder, tab: tab));
    final filtered = switch (tab) {
      'images' => items.where((i) => i.isImage).toList(),
      'files' => items.where((i) => !i.isImage).toList(),
      _ => items,
    };
    return LibraryPage(items: filtered);
  }
}

ProviderContainer _boot(MockCortexApi api) => ProviderContainer(
  overrides: [
    cortexApiProvider.overrideWithValue(api),
    appConfigProvider.overrideWith(() => _FixedConfig()),
  ],
);

class _FixedConfig extends AppConfigNotifier {
  @override
  AppConfig build() =>
      const AppConfig(useMock: true, baseUrl: '', offline: false);
}

Future<void> _pump(WidgetTester tester, ProviderContainer c) async {
  tester.view.physicalSize = const Size(1100, 1000);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    UncontrolledProviderScope(
      container: c,
      child: const MaterialApp(home: Scaffold(body: LibraryPageView())),
    ),
  );
  for (var i = 0; i < 6; i++) {
    await tester.pump(const Duration(milliseconds: 60));
  }
}

LibraryItem _item(
  String id,
  String name, {
  String mime = 'text/markdown',
  ChunkState state = ChunkState.ready,
  int chunks = 3,
}) => LibraryItem(
  id: id,
  blobHash: 'h$id',
  name: name,
  mime: mime,
  sizeBytes: 4096,
  chunkState: state,
  chunkCount: chunks,
);

void main() {
  testWidgets('空资料库要说清材料怎么放进来', (tester) async {
    final c = _boot(_LibApi());
    addTearDown(c.dispose);
    await _pump(tester, c);

    expect(find.text('资料库还是空的'), findsOneWidget);
    expect(
      find.textContaining('拖进对话里'),
      findsOneWidget,
      reason:
          '材料不是从这一页上传的，是从对话里拖进去的 —— 不说的话，'
          '用户在这一页上找不到任何入口，会以为功能没做完',
    );
    expect(tester.takeException(), isNull);
  });

  testWidgets('「提不出正文」照实说，不假装在切分', (tester) async {
    final c = _boot(
      _LibApi(
        items: [
          _item(
            'L1',
            '客户 POC 需求.docx',
            mime:
                'application/vnd.openxmlformats-officedocument'
                '.wordprocessingml.document',
            state: ChunkState.unsupported,
            chunks: 0,
          ),
        ],
      ),
    );
    addTearDown(c.dispose);
    await _pump(tester, c);

    expect(
      find.textContaining('提不出正文'),
      findsOneWidget,
      reason:
          '不说的话用户以为 docx 也能被检索到，然后因为模型查不到而'
          '认定资料库坏了 —— CLAUDE.md 约束 2 的界面版本',
    );
  });

  testWidgets('底下那句「不会自动进每一轮提示词」必须在', (tester) async {
    final c = _boot(_LibApi(items: [_item('L1', 'architecture.md')]));
    addTearDown(c.dispose);
    await _pump(tester, c);

    expect(
      find.textContaining('自动进每一轮提示词'),
      findsOneWidget,
      reason:
          '传一份规范进来最合理的预期是「它现在知道这份文件了」，而实际是'
          '按需检索。不说清的话，用户会因为模型没主动引用而以为上传失败了',
    );
  });

  test('换页签是重取，不是过滤手上这些', () async {
    final api = _LibApi(
      items: [
        _item('L1', 'a.md'),
        _item('L2', 'b.png', mime: 'image/png', state: ChunkState.unsupported),
      ],
    );
    final c = _boot(api);
    addTearDown(c.dispose);

    final n = c.read(libraryControllerProvider.notifier);
    await Future<void>.delayed(Duration.zero);
    await n.refresh();
    expect(c.read(libraryControllerProvider).items, hasLength(2));

    await n.setTab(LibraryTab.files);
    expect(
      api.calls.last.tab,
      'files',
      reason:
          '只过滤手上这一页的话，「文件」页会少掉所有还没翻到的 —— '
          '而它看起来是完整的',
    );
    expect(c.read(libraryControllerProvider).items.single.name, 'a.md');
  });

  test('这个部署没有资料库时不给重试按钮', () async {
    final c = _boot(_UnsupportedApi());
    addTearDown(c.dispose);

    final n = c.read(libraryControllerProvider.notifier);
    await Future<void>.delayed(Duration.zero);
    await n.refresh();

    final state = c.read(libraryControllerProvider);
    expect(state.unsupported, isTrue);
    expect(
      state.error,
      isNull,
      reason: '「这个部署没有资料库」不是故障 —— 摆成错误会让人去重试一件重试一百次也不会成的事',
    );
  });
}

class _UnsupportedApi extends MockCortexApi {
  _UnsupportedApi() : super(instant: true);

  @override
  Future<LibraryPage> library({
    int limit = 60,
    String? before,
    String? folder,
    String? tab,
  }) async => throw Exception('这个部署没有资料库：501');
}
