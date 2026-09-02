import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../api/api_exception.dart';
import '../../../models/attachment.dart';
import '../../../models/library_item.dart';
import '../../../state/app_providers.dart';
import '../../../state/attachment_controller.dart';
import '../../../state/library_controller.dart';
import '../../../state/library_save.dart';
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

class _CommittedAttachment extends ConsumerWidget {
  const _CommittedAttachment({required this.attachment, required this.extent});

  final Attachment attachment;

  /// 图片那一档的边长。文件卡片不受它影响（那是一行字，不是一张图）。
  final double extent;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    // 图那一支的右键菜单在 `_ImageThumb` 里（与图库共用 `ImageActions`），
    // 这里只补文件那一支
    if (attachment.isImage) {
      return _ImageThumb(
        hash: attachment.hash,
        label: attachment.displayName,
        extent: extent,
      );
    }
    return GestureDetector(
      onSecondaryTapDown: (d) =>
          _showFileMenu(context, ref, d.globalPosition, attachment),
      // 触摸设备上没有右键 —— 长按是同一个动作的另一只手
      onLongPressStart: (d) =>
          _showFileMenu(context, ref, d.globalPosition, attachment),
      child: _FileCard(
        title: attachment.displayName,
        subtitle: [
          kindLabel(attachment.kind),
          if (attachment.sizeBytes != null) formatBytes(attachment.sizeBytes),
        ].join(' · '),
      ),
    );
  }
}

/// 文件附件的右键菜单 —— 现在只有一项。
///
/// # 为什么要有这个入口
///
/// 资料库那一屏的空态写着「把文件拖进对话里发出去，它就会进资料库」，
/// 而附件那条路**一行都没有碰过 library**：`addToLibrary()` 在接口、
/// HTTP 实现、mock 里各写了一遍，零个调用点。于是生产上跑了一周多，
/// `library_items` 是 0 行 —— 一个只能看、进不去东西的资料库。
///
/// # 为什么是「手动收」而不是发出去就自动进
///
/// 那句空态描述的是自动进，但这个文件顶上的分工说的是另一回事：
/// **附件属于那一条消息，资料库属于你**。自动进的下场是资料库被一次性
/// 的截图和日志灌满，而它存在的理由正是「每一轮都可能被取用的背景材料」。
/// 所以留下手动这一档，并把那句空态改成实话。
Future<void> _showFileMenu(
  BuildContext context,
  WidgetRef ref,
  Offset at,
  Attachment attachment,
) async {
  final overlay = Overlay.of(context).context.findRenderObject()! as RenderBox;
  await showMenu<void>(
    context: context,
    position: RelativeRect.fromRect(
      at & const Size(1, 1),
      Offset.zero & overlay.size,
    ),
    items: [
      PopupMenuItem(
        onTap: () => _saveFileToLibrary(context, ref, attachment),
        child: const Text('存进资料库'),
      ),
    ],
  );
}

Future<void> _saveFileToLibrary(
  BuildContext context,
  WidgetRef ref,
  Attachment attachment,
) async {
  void said(String m) => ScaffoldMessenger.maybeOf(
    context,
  )?.showSnackBar(SnackBar(content: Text(m)));
  try {
    final item = await ref
        .read(cortexApiProvider)
        .addToLibrary(
          blobHash: attachment.hash,
          name: libraryNameFor(
            hash: attachment.hash,
            label: attachment.displayName,
          ),
        );
    ref.invalidate(libraryControllerProvider);
    // **切不出正文的要当场说。** pdf/docx/图片落的是 `unsupported`，
    // 不说的话用户以为收进去就能被检索到，然后因为模型查不到而认定
    // 资料库坏了 —— 资料库那一屏为这件事专门用了琥珀色，这里不能更含糊
    said(
      item.chunkState == ChunkState.unsupported
          ? '已收进资料库。不过这类文件还提不出正文，模型检索不到它。'
          : '已收进资料库，模型需要时会自己去检索它。',
    );
  } on CortexApiException catch (e) {
    said(e.isUnsupported ? '这个部署没有资料库。' : e.message);
  } on Object catch (e) {
    said('$e');
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
        // 主题里的 success，不写死色值：单值绿在浅色底上对比不够，
        // 而这个 token 深浅两套已经配好对
        theme.cortex.success,
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
