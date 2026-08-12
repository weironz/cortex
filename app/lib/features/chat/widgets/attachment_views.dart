import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../models/attachment.dart';
import '../../../state/app_providers.dart';
import '../../../state/attachment_controller.dart';

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
          for (final a in attachments) _CommittedAttachment(attachment: a),
        ],
      ),
    );
  }
}

class _CommittedAttachment extends StatelessWidget {
  const _CommittedAttachment({required this.attachment});

  final Attachment attachment;

  @override
  Widget build(BuildContext context) {
    return attachment.isImage
        ? _ImageThumb(hash: attachment.hash, label: attachment.displayName)
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
  const _ImageThumb({required this.hash, required this.label});

  final String hash;
  final String label;

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
      child: Container(
        width: 132,
        height: 96,
        decoration: BoxDecoration(
          color: scheme.surfaceContainerHigh,
          borderRadius: BorderRadius.circular(9),
          border: Border.all(color: scheme.outlineVariant),
        ),
        clipBehavior: Clip.antiAlias,
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
                fit: BoxFit.cover,
                // Decoding at display size instead of full resolution: a 12 MP
                // photo would otherwise sit in the raster cache at ~48 MB.
                cacheWidth: 264,
                errorBuilder: (_, _, _) => Center(
                  child: Icon(
                    Icons.broken_image_outlined,
                    size: 18,
                    color: scheme.onSurfaceVariant,
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
        borderRadius: BorderRadius.circular(9),
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
        borderRadius: BorderRadius.circular(9),
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
