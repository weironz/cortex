/// 图片规格 —— **只有两项，因为只有这两项是真的**。
///
/// # 少画的那两个控件
///
/// ChatGPT 那个面板上有 `背景 / 生成数量 / 质量 / 图片尺寸`。质量与背景是
/// OpenAI **官方 images API** 的参数，而我们三条生图协议里没有一条走它
///（DashScope 原生 / Gemini / 中转站的聊天协议），所以那两个控件发不出去。
///
/// 画一个发不出去的控件，就是 CLAUDE.md 第 2 条不可违反约束的界面版本：
/// 界面替模型答应了一件它做不到的事。
///
/// # 剩下这两项也不是处处一样真
///
/// | 来源 | 尺寸 | 数量 |
/// |---|---|---|
/// | 通义千问（DashScope 原生） | 真参数 | 真参数，一次出多张 |
/// | Google（Gemini） | 映射成宽高比 + 档位 | 服务端连发 n 次凑的 |
/// | 自定义端点（中转站/网关） | **只能拼进提示词**，认不认不由我们说 | 服务端连发 n 次凑的 |
///
/// 差别要**写在控件旁边**，而不是让三种来源共用一套看起来一样硬的开关 ——
/// 「我设了 1024×1024，出来却不是」在中转站上是常态，用户得知道那不是 bug。
library;

import 'package:flutter/material.dart';

import '../../../core/theme.dart';

/// 尺寸预设。
///
/// 这一组是照 ChatGPT 那三档挑的，同时**能干净地映射到 Gemini 的档位**
///（1:1 / 3:2 / 2:3）—— 换成 1280×720 之类的话，`gemini_shape` 那侧算不出
/// 比例就两个字段都不发，用户设的尺寸静默失效。
const List<({String? value, String label})> kImageSizes = [
  (value: null, label: '自动'),
  (value: '1024*1024', label: '1024×1024'),
  (value: '1536*1024', label: '1536×1024'),
  (value: '1024*1536', label: '1024×1536'),
];

/// 一次最多几张。
///
/// 4 而不是 DashScope 支持的 6：另外两条协议是**连发**凑的，
/// 6 张就是 6 次几十秒的调用 —— 在一个同步等结果的界面上太久了。
const int kMaxImageCount = 4;

class ImageSpecSheet extends StatefulWidget {
  const ImageSpecSheet({
    super.key,
    required this.size,
    required this.count,
    required this.nativeMultiImage,
    required this.customEndpoint,
  });

  final String? size;
  final int count;

  /// 这条来源一次请求就能出多张吗（只有 DashScope 原生能）。
  /// 不能的话，数量由服务端连发凑 —— 那件事要说出来，因为它按次收钱。
  final bool nativeMultiImage;

  /// 这条来源填了自己的端点（中转站/网关）。尺寸在那儿只是提示词里的一句话。
  final bool customEndpoint;

  @override
  State<ImageSpecSheet> createState() => _ImageSpecSheetState();
}

class _ImageSpecSheetState extends State<ImageSpecSheet> {
  late String? _size = widget.size;
  late int _count = widget.count;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return SafeArea(
      child: Padding(
        padding: const EdgeInsets.fromLTRB(20, 4, 20, 16),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text('图片规格', style: theme.textTheme.titleSmall),
            const SizedBox(height: 16),

            _label(context, '图片尺寸'),
            const SizedBox(height: 6),
            Wrap(
              spacing: 8,
              runSpacing: 8,
              children: [
                for (final s in kImageSizes)
                  ChoiceChip(
                    key: ValueKey('size:${s.value ?? "auto"}'),
                    label: Text(s.label),
                    selected: _size == s.value,
                    onSelected: (_) => setState(() => _size = s.value),
                  ),
              ],
            ),
            if (widget.customEndpoint) ...[
              const SizedBox(height: 6),
              _note(
                context,
                '这条来源自己填了端点，尺寸只能写进提示词 —— '
                '对方认不认不由我们说，出来的图未必是这个尺寸。',
                key: const ValueKey('note:size-best-effort'),
              ),
            ],

            const SizedBox(height: 18),
            _label(context, '生成数量'),
            const SizedBox(height: 6),
            Row(
              children: [
                Expanded(
                  child: Slider(
                    value: _count.toDouble(),
                    min: 1,
                    max: kMaxImageCount.toDouble(),
                    divisions: kMaxImageCount - 1,
                    label: '$_count',
                    onChanged: (v) => setState(() => _count = v.round()),
                  ),
                ),
                SizedBox(
                  width: 28,
                  child: Text('$_count', style: theme.textTheme.bodySmall),
                ),
              ],
            ),
            // **钱要说在按下去之前。** 生图按张计费，而这个滑块是这一页
            // 唯一一个能把一次操作的花费翻两番的控件
            _note(
              context,
              _count == 1 ? '生图按张计费。' : '生图按张计费 —— $_count 张就是 $_count 倍的钱和时间。',
              key: const ValueKey('note:cost'),
            ),
            if (!widget.nativeMultiImage && _count > 1) ...[
              const SizedBox(height: 4),
              _note(
                context,
                '这条来源一次只出一张，$_count 张是服务端连发 $_count 次凑的 —— '
                '所以要等更久，而且可能只成几张。',
                key: const ValueKey('note:fanout'),
              ),
            ],

            const SizedBox(height: 20),
            Row(
              mainAxisAlignment: MainAxisAlignment.end,
              children: [
                TextButton(
                  onPressed: () => Navigator.of(context).pop(),
                  child: const Text('取消'),
                ),
                const SizedBox(width: 8),
                FilledButton(
                  onPressed: () =>
                      Navigator.of(context).pop((size: _size, count: _count)),
                  child: const Text('用这个规格'),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  Widget _label(BuildContext context, String text) =>
      Text(text, style: Theme.of(context).textTheme.labelMedium);

  Widget _note(BuildContext context, String text, {Key? key}) => Text(
    text,
    key: key,
    style: Theme.of(context).textTheme.labelSmall?.copyWith(
      color: Theme.of(context).cortex.foregroundTertiary,
    ),
  );
}

/// 打开规格面板。`null` = 用户取消了。
Future<({String? size, int count})?> showImageSpecSheet(
  BuildContext context, {
  required String? size,
  required int count,
  required bool nativeMultiImage,
  required bool customEndpoint,
}) => showModalBottomSheet<({String? size, int count})>(
  context: context,
  showDragHandle: true,
  builder: (_) => ImageSpecSheet(
    size: size,
    count: count,
    nativeMultiImage: nativeMultiImage,
    customEndpoint: customEndpoint,
  ),
);
