/// 点开一张图 —— 放大，以及那五个动作。
///
/// # 「编辑」为什么叫「以此为提示词重画」
///
/// ChatGPT 那个按钮叫「编辑」，做的是 img2img：把原图连同新指令一起发回去，
/// 在**这张图上**改。我们三条生图协议都没有带图输入的那条路，所以做不到。
///
/// 沿用「编辑」这个名字的代价很具体：用户会写「把领结换成红色」，
/// 而拿到的是一张构图完全不同的新图 —— 他会以为模型没听懂，
/// 而实际上是我们从来没把那张图发过去。名字说清做的是什么，
/// 他就会改写整句提示词。
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../core/copy_image.dart';
import '../../../core/save_file.dart';
import '../../../core/theme.dart';
import '../../../models/generated_image.dart';
import '../../../state/app_providers.dart';
import '../../../state/blob_bytes.dart';

/// 打开大图。返回一句提示词 = 用户点了「以此为提示词重画」。
Future<String?> showImageViewer(BuildContext context, GeneratedImage image) =>
    showDialog<String>(
      context: context,
      // 大图要占住整个窗口，默认那个 `insetPadding` 会留一圈用不上的边
      builder: (_) => Dialog(
        insetPadding: const EdgeInsets.all(24),
        clipBehavior: Clip.antiAlias,
        child: ImageViewer(image: image),
      ),
    );

class ImageViewer extends ConsumerStatefulWidget {
  const ImageViewer({super.key, required this.image});

  final GeneratedImage image;

  @override
  ConsumerState<ImageViewer> createState() => _ImageViewerState();
}

class _ImageViewerState extends ConsumerState<ImageViewer> {
  /// 刚做完的那件事。**一次只留一条** —— 五个按钮各挂一条提示的话，
  /// 底下会堆起一摞过时的话。
  String? _said;
  bool _busy = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final bytes = ref.watch(blobBytesProvider(widget.image.hash));

    return ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: 1100, maxHeight: 900),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          _header(context),
          Flexible(
            child: Container(
              color: theme.colorScheme.surfaceContainerHighest,
              width: double.infinity,
              child: bytes.when(
                loading: () => const SizedBox(
                  height: 320,
                  child: Center(child: CircularProgressIndicator()),
                ),
                error: (e, _) => SizedBox(
                  height: 320,
                  child: Center(
                    child: Text(
                      '取不到这张图：$e',
                      style: theme.textTheme.bodySmall?.copyWith(
                        color: theme.colorScheme.error,
                      ),
                    ),
                  ),
                ),
                // 可缩放、可拖。**这就是「点击放大」那一条** ——
                // 一张 1024 的图在窗口里是缩着的，看细节要能推近
                data: (b) => InteractiveViewer(
                  maxScale: 6,
                  child: Center(child: Image.memory(b, fit: BoxFit.contain)),
                ),
              ),
            ),
          ),
          _actions(context, bytes.value),
        ],
      ),
    );
  }

  Widget _header(BuildContext context) {
    final theme = Theme.of(context);
    final img = widget.image;
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 12, 8, 8),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  img.prompt,
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.bodyMedium,
                ),
                const SizedBox(height: 2),
                Text(
                  // 型号与尺寸回答「这张多少钱、还画得出来吗」。
                  // 时间解析不出来就不画 —— 编一个「刚刚」比留白更糟
                  [
                    img.model,
                    if (img.size != null) img.size!.replaceAll('*', '×'),
                  ].join(' · '),
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: theme.cortex.foregroundTertiary,
                  ),
                ),
              ],
            ),
          ),
          IconButton(
            tooltip: '关闭',
            iconSize: 18,
            onPressed: () => Navigator.of(context).maybePop(),
            icon: const Icon(Icons.close_rounded),
          ),
        ],
      ),
    );
  }

  Widget _actions(BuildContext context, Uint8List? bytes) {
    final theme = Theme.of(context);
    final ready = bytes != null && !_busy;
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 8, 12, 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Wrap(
            spacing: 8,
            runSpacing: 8,
            children: [
              TextButton.icon(
                key: const ValueKey('viewer:copy'),
                onPressed: ready ? () => _copyImage(bytes) : null,
                icon: const Icon(Icons.copy_rounded, size: 16),
                label: const Text('复制图片'),
              ),
              TextButton.icon(
                key: const ValueKey('viewer:download'),
                onPressed: ready ? () => _download(bytes) : null,
                icon: const Icon(Icons.download_rounded, size: 16),
                label: const Text('下载'),
              ),
              TextButton.icon(
                key: const ValueKey('viewer:link'),
                onPressed: _busy ? null : _copyLink,
                icon: const Icon(Icons.link_rounded, size: 16),
                label: const Text('复制链接'),
              ),
              TextButton.icon(
                key: const ValueKey('viewer:reprompt'),
                onPressed: () => Navigator.of(context).pop(widget.image.prompt),
                icon: const Icon(Icons.brush_outlined, size: 16),
                label: const Text('以此为提示词重画'),
              ),
            ],
          ),
          if (_said != null) ...[
            const SizedBox(height: 8),
            Text(
              _said!,
              key: const ValueKey('viewer:said'),
              style: theme.textTheme.labelSmall?.copyWith(
                color: theme.cortex.foregroundTertiary,
              ),
            ),
          ],
        ],
      ),
    );
  }

  Future<void> _copyImage(Uint8List bytes) async {
    setState(() => _busy = true);
    final ok = await copyImageToClipboard(bytes);
    if (!mounted) return;
    setState(() {
      _busy = false;
      // 失败要**说出来**。静默当成功的表现是「粘出来还是上一次的东西」，
      // 而用户根本不会怀疑到这一步
      _said = ok
          ? '图片已经放进剪贴板了。'
          : '这台机器上没能放进剪贴板 —— 可以用「下载」，或者再试一次'
                '（剪贴板同一时刻只能被一个程序占着）。';
    });
  }

  Future<void> _download(Uint8List bytes) async {
    setState(() => _busy = true);
    // 文件名带上哈希前八位：同一句提示词画出来的几张，名字一样就会互相覆盖
    final name = 'cortex-${widget.image.hash.substring(0, 8)}.png';
    final ok = await saveBytesAs(bytes, name);
    if (!mounted) return;
    setState(() {
      _busy = false;
      _said = ok ? '存好了：$name' : '没能存下来。';
    });
  }

  Future<void> _copyLink() async {
    setState(() => _busy = true);
    try {
      final link = await ref.read(cortexApiProvider).blobUrl(widget.image.hash);
      await Clipboard.setData(ClipboardData(text: link.url));
      if (!mounted) return;
      final mins = (link.expiresInSecs / 60).round();
      setState(() {
        _busy = false;
        // **过期这件事必须一路说到这儿。** 一条十五分钟后 404 的链接
        // 比「复制不了」更坏：他发出去就不管了，只会以为是对方打不开
        _said = '链接已复制 —— 它 $mins 分钟后失效，要长期分享请用「下载」。';
      });
    } on CortexApiException catch (e) {
      if (!mounted) return;
      setState(() {
        _busy = false;
        // 501 = 这个部署的对象存储签不出 URL（本地文件系统那种）。
        // 说清是**部署**的能力，不是这次操作失败了
        _said = e.isUnsupported
            ? '这个部署签不出直链（对象存储没配或者不支持）—— 用「下载」或「复制图片」。'
            : e.message;
      });
    } on Object catch (e) {
      if (!mounted) return;
      setState(() {
        _busy = false;
        _said = '$e';
      });
    }
  }
}
