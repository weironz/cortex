/// 点开一张图 —— 放大、转正，以及那几个动作。
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
///
/// # ⚠️ 缩放 / 旋转 / 翻转**只影响这里看到的样子**
///
/// 它们不改字节。复制、另存、分享出去的都是**原图** —— 要让变换落到文件里
/// 得重新编码，而那还要先回答「另存的是原图还是我看到的样子」。
/// 这一点必须写在界面上：转完再另存却得到一张没转的图，是这条路上最容易
/// 让人白忙一场的地方。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/theme.dart';
import '../../../models/generated_image.dart';
import '../../../state/blob_bytes.dart';
import 'image_actions.dart';

/// 查看器要知道的那几件事。
///
/// # 为什么不直接吃 [GeneratedImage]
///
/// 对话里那张图**不是**画廊的一行 —— 它是一个附件，手上只有哈希与文件名。
/// 硬塞进 `GeneratedImage` 的话，得给 id / prompt / model 编几个假值，
/// 而界面会把那些假值当真画出来（「型号：unknown」）。
///
/// 少的那几样在这里是 `null`，画不出来就不画。
class ViewerImage {
  const ViewerImage({
    required this.hash,
    this.galleryId,
    this.prompt,
    this.model,
    this.size,
    this.shareUrl,
    this.lookupByHash = false,
  });

  /// 从画廊那一行来 —— 什么都知道。
  factory ViewerImage.fromGallery(GeneratedImage i) => ViewerImage(
    hash: i.hash,
    galleryId: i.id,
    prompt: i.prompt,
    model: i.model,
    size: i.size,
    shareUrl: i.shareUrl,
  );

  final String hash;
  final String? galleryId;
  final String? prompt;
  final String? model;
  final String? size;
  final String? shareUrl;

  /// 见 [ImageActions.lookupByHash]。
  final bool lookupByHash;
}

/// 打开大图。返回一句提示词 = 用户点了「以此为提示词重画」。
Future<String?> showImageViewer(
  BuildContext context,
  ViewerImage image, {
  VoidCallback? onChanged,
}) => showDialog<String>(
  context: context,
  // 大图要占住整个窗口，默认那个 `insetPadding` 会留一圈用不上的边
  builder: (_) => Dialog(
    insetPadding: const EdgeInsets.all(24),
    clipBehavior: Clip.antiAlias,
    child: ImageViewer(image: image, onChanged: onChanged),
  ),
);

class ImageViewer extends ConsumerStatefulWidget {
  const ImageViewer({super.key, required this.image, this.onChanged});

  final ViewerImage image;

  /// 分享状态或图库内容变了 —— 调用方据此刷新。
  final VoidCallback? onChanged;

  @override
  ConsumerState<ImageViewer> createState() => _ImageViewerState();
}

class _ImageViewerState extends ConsumerState<ImageViewer> {
  /// 刚做完的那件事。**一次只留一条** —— 每个按钮各挂一条提示的话，
  /// 底下会堆起一摞过时的话。
  String? _said;

  final _tc = TransformationController();

  /// 转了几个 90°。
  int _quarter = 0;

  /// 水平 / 垂直翻转。
  bool _flipH = false;
  bool _flipV = false;

  bool get _transformed => _quarter != 0 || _flipH || _flipV;

  @override
  void dispose() {
    _tc.dispose();
    super.dispose();
  }

  void _zoom(double factor) {
    // 在**当前**缩放上乘一个系数，并夹在 InteractiveViewer 自己那对上下限里
    // —— 不夹的话按钮能把图缩到看不见，而手势那条路是有限制的，
    // 两条路给出不一样的边界读起来就是「有时候能有时候不能」
    final now = _tc.value.getMaxScaleOnAxis();
    final next = (now * factor).clamp(1.0, 6.0);
    _tc.value = Matrix4.identity()..scaleByDouble(next, next, next, 1);
  }

  ImageActions get _actions => ImageActions(
    ref: ref,
    hash: widget.image.hash,
    galleryId: widget.image.galleryId,
    lookupByHash: widget.image.lookupByHash,
    prompt: widget.image.prompt,
    shareUrl: widget.image.shareUrl,
    said: (m) => setState(() => _said = m),
    onRemoved: () {
      widget.onChanged?.call();
      if (mounted) Navigator.of(context).maybePop();
    },
  );

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
                data: (b) => GestureDetector(
                  onSecondaryTapDown: (d) =>
                      showImageContextMenu(context, d.globalPosition, _actions),
                  child: InteractiveViewer(
                    transformationController: _tc,
                    maxScale: 6,
                    child: Center(
                      child: Transform.flip(
                        flipX: _flipH,
                        flipY: _flipV,
                        child: RotatedBox(
                          quarterTurns: _quarter,
                          child: Image.memory(b, fit: BoxFit.contain),
                        ),
                      ),
                    ),
                  ),
                ),
              ),
            ),
          ),
          _toolbar(context),
          _actionsRow(context),
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
                // 提示词不知道就不占这一行（对话里那张是附件，没有它）
                if (img.prompt != null)
                  Text(
                    img.prompt!,
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                    style: theme.textTheme.bodyMedium,
                  ),
                const SizedBox(height: 2),
                Row(
                  children: [
                    Text(
                      // 型号与尺寸回答「这张多少钱、还画得出来吗」。
                      // 不知道的就不画 —— 编一个「unknown」比留白更糟
                      [
                        if (img.model != null) img.model!,
                        if (img.size != null) img.size!.replaceAll('*', '×'),
                      ].join(' · '),
                      style: theme.textTheme.labelSmall?.copyWith(
                        color: theme.cortex.foregroundTertiary,
                      ),
                    ),
                    // 已经分享出去的要**一眼看得见**，否则就是一批没人记得
                    // 的公开 URL
                    if (img.shareUrl != null) ...[
                      const SizedBox(width: 8),
                      Icon(
                        Icons.public,
                        size: 13,
                        color: theme.cortex.foregroundTertiary,
                      ),
                      const SizedBox(width: 3),
                      Text(
                        '已分享',
                        key: const ValueKey('viewer:shared'),
                        style: theme.textTheme.labelSmall?.copyWith(
                          color: theme.cortex.foregroundTertiary,
                        ),
                      ),
                    ],
                  ],
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

  /// 看图的那一排：缩放、旋转、翻转、重置。
  Widget _toolbar(BuildContext context) {
    final theme = Theme.of(context);
    Widget btn(String tip, IconData icon, VoidCallback onTap, {Key? key}) =>
        IconButton(
          key: key,
          tooltip: tip,
          iconSize: 18,
          visualDensity: VisualDensity.compact,
          onPressed: onTap,
          icon: Icon(icon),
        );

    return Container(
      decoration: BoxDecoration(
        border: Border(
          top: BorderSide(color: theme.colorScheme.outlineVariant),
        ),
      ),
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
      child: Row(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          btn('缩小', Icons.zoom_out_rounded, () => _zoom(1 / 1.4)),
          btn('放大', Icons.zoom_in_rounded, () => _zoom(1.4)),
          const SizedBox(width: 8),
          btn(
            '向左转 90°',
            Icons.rotate_left_rounded,
            () => setState(() => _quarter = (_quarter + 3) % 4),
            key: const ValueKey('viewer:rotl'),
          ),
          btn(
            '向右转 90°',
            Icons.rotate_right_rounded,
            () => setState(() => _quarter = (_quarter + 1) % 4),
            key: const ValueKey('viewer:rotr'),
          ),
          const SizedBox(width: 8),
          btn(
            '水平翻转',
            Icons.swap_horiz_rounded,
            () => setState(() => _flipH = !_flipH),
            key: const ValueKey('viewer:fliph'),
          ),
          btn(
            '垂直翻转',
            Icons.swap_vert_rounded,
            () => setState(() => _flipV = !_flipV),
            key: const ValueKey('viewer:flipv'),
          ),
          const SizedBox(width: 8),
          btn('复原', Icons.restart_alt_rounded, () {
            setState(() {
              _quarter = 0;
              _flipH = false;
              _flipV = false;
            });
            _tc.value = Matrix4.identity();
          }),
        ],
      ),
    );
  }

  Widget _actionsRow(BuildContext context) {
    final theme = Theme.of(context);
    final a = _actions;
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
                onPressed: a.copyImage,
                icon: const Icon(Icons.copy_rounded, size: 16),
                label: const Text('复制图片'),
              ),
              TextButton.icon(
                key: const ValueKey('viewer:save'),
                onPressed: a.saveAs,
                icon: const Icon(Icons.download_rounded, size: 16),
                label: const Text('另存为…'),
              ),
              if (a.inGallery)
                TextButton.icon(
                  key: const ValueKey('viewer:link'),
                  onPressed: a.copyLink,
                  icon: const Icon(Icons.link_rounded, size: 16),
                  label: const Text('复制图片链接'),
                ),
              if (widget.image.shareUrl != null)
                TextButton.icon(
                  key: const ValueKey('viewer:unshare'),
                  onPressed: a.unshare,
                  icon: const Icon(Icons.public_off_rounded, size: 16),
                  label: const Text('停止分享'),
                ),
              // 不知道原来那句话就不给这个按钮 —— 一个按下去什么都不
              // 发生的按钮，比没有这个按钮更让人费解
              if (widget.image.prompt != null)
                TextButton.icon(
                  key: const ValueKey('viewer:reprompt'),
                  onPressed: () =>
                      Navigator.of(context).pop(widget.image.prompt),
                  icon: const Icon(Icons.brush_outlined, size: 16),
                  label: const Text('以此为提示词重画'),
                ),
            ],
          ),
          // ⚠️ 转过之后要**说清导出的是原图**。不说的话，用户转正再另存，
          // 得到的还是歪的 —— 而他会以为是保存坏了
          if (_transformed) ...[
            const SizedBox(height: 8),
            Text(
              '这里的旋转与翻转只影响预览。复制 / 另存 / 分享出去的都是原图。',
              key: const ValueKey('viewer:preview-only'),
              style: theme.textTheme.labelSmall?.copyWith(
                color: theme.cortex.foregroundTertiary,
              ),
            ),
          ],
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
}
