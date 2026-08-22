import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../models/attachment.dart';
import '../../../state/app_providers.dart';
import '../../../state/attachment_controller.dart';
import '../../../core/theme.dart';
import '../../images/widgets/image_actions.dart';
import '../../images/widgets/image_thumb.dart' show decodeWidthFor;
import '../../images/widgets/image_viewer.dart';

/// Thumbnails for attachments already committed to a message.
class AttachmentStrip extends StatelessWidget {
  const AttachmentStrip({
    super.key,
    required this.attachments,
    this.alignEnd = false,
  });

  final List<Attachment> attachments;

  /// User bubbles are right-aligned; assistant blocks are not.
  final bool alignEnd;

  /// 这一条里的图画多大。
  ///
  /// # ⚠️ 「模型画出来的」与「我传上去的」不是一个东西
  ///
  /// 从前两者都是 132×96、`BoxFit.cover`（**裁掉**）。在那个尺寸上，一张
  /// 生成图画得对不对根本看不出来 —— 而那正是用户唯一想知道的事。
  ///
  /// 所以按两件事分档：
  ///
  /// * **assistant 那一侧是内容**：一张就画大（384），几张一起出是一份
  ///   contact sheet，缩到 224 好横着比 —— 四张 384 会砌成一堵墙。
  /// * **user 那一侧是材料**：他自己刚传上去的，知道是什么，
  ///   给到看得清就够（160）。
  ///
  /// 一律 `BoxFit.contain`：生成图裁一刀就不是那张图了。
  double get _extent {
    if (alignEnd) return 160;
    return attachments.length == 1 ? 384 : 224;
  }

  @override
  Widget build(BuildContext context) {
    if (attachments.isEmpty) return const SizedBox.shrink();
    return Padding(
      padding: const EdgeInsets.only(bottom: 6),
      child: Wrap(
        spacing: 6,
        runSpacing: 6,
        alignment: alignEnd ? WrapAlignment.end : WrapAlignment.start,
        children: [
          for (final a in attachments)
            _CommittedAttachment(attachment: a, extent: _extent),
        ],
      ),
    );
  }
}

class _CommittedAttachment extends StatelessWidget {
  const _CommittedAttachment({required this.attachment, required this.extent});

  final Attachment attachment;

  /// 图片那一档的边长。文件卡片不受它影响（那是一行字，不是一张图）。
  final double extent;

  @override
  Widget build(BuildContext context) {
    return attachment.isImage
        ? _ImageThumb(
            hash: attachment.hash,
            label: attachment.displayName,
            extent: extent,
          )
        : _FileCard(
            title: attachment.displayName,
            subtitle: [
              kindLabel(attachment.kind),
              if (attachment.sizeBytes != null)
                formatBytes(attachment.sizeBytes),
            ].join(' · '),
          );
  }
}

/// Fetches blob bytes once per hash and renders them.
///
/// Bytes rather than an `Image.network` URL because the mock source has no
/// origin to hand out — going through `CortexApi.blobBytes` keeps this widget
/// working identically on both, which is the whole point of the mock.
///
/// The result is cached by hash for the process lifetime. Content addressing
/// makes that trivially safe: the same hash can never mean different bytes, so
/// there is no invalidation problem to get wrong.
class _ImageThumb extends ConsumerStatefulWidget {
  const _ImageThumb({
    required this.hash,
    required this.label,
    required this.extent,
  });

  final String hash;
  final String label;

  /// 这张图占多大（正方形）。见 [AttachmentStrip._extent]。
  final double extent;

  @override
  ConsumerState<_ImageThumb> createState() => _ImageThumbState();
}

class _ImageThumbState extends ConsumerState<_ImageThumb> {
  static final Map<String, Uint8List> _cache = {};

  Uint8List? _bytes;
  String? _error;

  @override
  void initState() {
    super.initState();
    _fetch();
  }

  Future<void> _fetch() async {
    final cached = _cache[widget.hash];
    if (cached != null) {
      setState(() => _bytes = cached);
      return;
    }
    try {
      final bytes = await ref.read(cortexApiProvider).blobBytes(widget.hash);
      _cache[widget.hash] = bytes;
      if (!mounted) return;
      setState(() => _bytes = bytes);
    } on Object catch (e) {
      if (!mounted) return;
      setState(() => _error = '$e');
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;

    if (_error != null) {
      return _FileCard(
        title: widget.label,
        subtitle: '取不到图片',
        tone: scheme.error,
      );
    }

    return Tooltip(
      message: widget.label,
      // 与图库那一格（`ImageThumb`）**同一副骨架**：`Material` + `InkWell`。
      //
      // 从前这里是裸的 `GestureDetector`，没有按下去的水波纹 —— 而它现在
      // 是可以点的（点开大图）。一个能点却没有任何反馈的方块，用户要试
      // 一下才知道点不点得动。
      child: Material(
        color: scheme.surfaceContainerHigh,
        clipBehavior: Clip.antiAlias,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
          side: BorderSide(color: scheme.outlineVariant),
        ),
        child: InkWell(
          // 点一下放大看原图。缩略图再大也只是「够判断画对没有」，
          // 而挑细节要的是原尺寸
          onTap: () => showImageViewer(
            context,
            ViewerImage(
              hash: widget.hash,
              // 手上只有哈希。agent 画的那些都在图库里，所以**允许按哈希
              // 去问一次**：不问的话，同一张图在对话里右键出来的菜单比
              // 图库里少三项，而用户分不清那是两个东西
              lookupByHash: true,
            ),
          ),
          // 对话里那张图与图库里那张**共用同一份动作**（见 `ImageActions`）
          onSecondaryTapDown: (d) => showImageContextMenu(
            context,
            d.globalPosition,
            ImageActions(
              ref: ref,
              hash: widget.hash,
              lookupByHash: true,
              said: (m) => ScaffoldMessenger.maybeOf(
                context,
              )?.showSnackBar(SnackBar(content: Text(m))),
            ),
          ),
          child: SizedBox(
            // 正方形，**不按图的比例撑开**：比例得等字节到齐才知道，
            // 而按到齐之后再撑，整段对话会在图加载完的那一刻整体跳一下
            width: widget.extent,
            height: widget.extent,
            child: _bytes == null
                ? const Center(
                    child: SizedBox(
                      width: 15,
                      height: 15,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    ),
                  )
                : Image.memory(
                    _bytes!,
                    // **不裁。** 生成图裁一刀就不是那张图了 —— 竖版被切成
                    // 方的之后，用户看到的构图跟模型画的不是一回事
                    fit: BoxFit.contain,
                    // 按显示尺寸解码，不按原图：一张 12 MP 的照片全分辨率
                    // 解出来在光栅缓存里是 48 MB
                    cacheWidth: decodeWidthFor(context, widget.extent),
                    errorBuilder: (_, _, _) => Center(
                      child: Icon(
                        Icons.broken_image_outlined,
                        size: 18,
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                  ),
          ),
        ),
      ),
    );
  }
}

class _FileCard extends StatelessWidget {
  const _FileCard({required this.title, required this.subtitle, this.tone});

  final String title;
  final String subtitle;
  final Color? tone;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final color = tone ?? scheme.onSurfaceVariant;

    return Container(
      width: 208,
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHigh,
        borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
        border: Border.all(color: scheme.outlineVariant),
      ),
      child: Row(
        children: [
          Icon(Icons.insert_drive_file_outlined, size: 17, color: color),
          const SizedBox(width: 9),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              mainAxisSize: MainAxisSize.min,
              children: [
                Text(
                  title,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.labelMedium,
                ),
                Text(subtitle, style: theme.textTheme.labelSmall),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

/// The composer's tray: files being uploaded, ready, or failed.
class PendingAttachmentTray extends ConsumerWidget {
  const PendingAttachmentTray({super.key, required this.sessionId});

  final String sessionId;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final queue = ref.watch(attachmentQueueProvider);
    final items = queue[sessionId] ?? const <PendingAttachment>[];
    if (items.isEmpty) return const SizedBox.shrink();

    final notifier = ref.read(attachmentQueueProvider.notifier);

    return Padding(
      padding: const EdgeInsets.only(bottom: 8),
      child: Wrap(
        spacing: 6,
        runSpacing: 6,
        children: [
          for (final item in items)
            _PendingCard(
              key: ValueKey(item.id),
              item: item,
              onRemove: () => notifier.remove(sessionId, item.id),
              onRetry: () => notifier.retry(sessionId, item.id),
            ),
        ],
      ),
    );
  }
}

class _PendingCard extends StatelessWidget {
  const _PendingCard({
    super.key,
    required this.item,
    required this.onRemove,
    required this.onRetry,
  });

  final PendingAttachment item;
  final VoidCallback onRemove;
  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;

    final (Color accent, String status) = switch (item.status) {
      UploadStatus.uploading => (
        scheme.secondary,
        // At 100% the bytes are handed over but the server has not answered.
        // Saying "已上传" here would be a lie on Web, where the browser has
        // only just started the actual transfer.
        item.progress >= 1
            ? '等待服务端登记…'
            : '上传中 ${(item.progress * 100).round()}%',
      ),
      UploadStatus.ready => (
        const Color(0xFF2E9E5B),
        formatBytes(item.attachment?.sizeBytes ?? item.total),
      ),
      UploadStatus.failed => (scheme.error, item.error ?? '上传失败'),
    };

    return Container(
      width: 236,
      padding: const EdgeInsets.fromLTRB(10, 8, 4, 8),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHigh,
        borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
        border: Border.all(color: accent.withValues(alpha: 0.35)),
      ),
      child: Row(
        children: [
          Icon(
            switch (item.status) {
              UploadStatus.failed => Icons.error_outline_rounded,
              UploadStatus.ready when item.isImage => Icons.image_outlined,
              UploadStatus.ready => Icons.check_circle_outline_rounded,
              UploadStatus.uploading => Icons.upload_file_rounded,
            },
            size: 16,
            color: accent,
          ),
          const SizedBox(width: 9),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              mainAxisSize: MainAxisSize.min,
              children: [
                Text(
                  item.filename,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.labelMedium,
                ),
                Text(
                  status,
                  maxLines: 2,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.labelSmall?.copyWith(color: accent),
                ),
                if (item.status == UploadStatus.uploading) ...[
                  const SizedBox(height: 5),
                  ClipRRect(
                    // **不走圆角那五阶**：这是个胶囊，不是一个面。
                    // 条子高 3，半高就是 1.5 —— 2 已经把两端切成半圆了，
                    // 换成最小的 radiusSm（6）不会更圆，只会白绕一圈
                    borderRadius: BorderRadius.circular(2),
                    child: LinearProgressIndicator(
                      // Indeterminate once everything is handed over: the
                      // remaining wait is the server's, and its length is
                      // unknown to us.
                      value: item.progress >= 1 ? null : item.progress,
                      minHeight: 3,
                      backgroundColor: scheme.surfaceContainerHighest,
                    ),
                  ),
                ],
              ],
            ),
          ),
          if (item.status == UploadStatus.failed)
            IconButton(
              onPressed: onRetry,
              iconSize: 15,
              visualDensity: VisualDensity.compact,
              tooltip: '重试',
              icon: const Icon(Icons.refresh_rounded),
            ),
          IconButton(
            onPressed: onRemove,
            iconSize: 15,
            visualDensity: VisualDensity.compact,
            tooltip: '移除',
            icon: const Icon(Icons.close_rounded),
          ),
        ],
      ),
    );
  }
}
