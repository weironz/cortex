/// 画廊里的一格。
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/theme.dart';
import '../../../models/generated_image.dart';
import '../../../state/blob_bytes.dart';

class ImageThumb extends ConsumerWidget {
  const ImageThumb({super.key, required this.image, required this.onTap});

  final GeneratedImage image;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final bytes = ref.watch(blobBytesProvider(image.hash));

    return Tooltip(
      // 提示词可以很长，`Tooltip` 自己会换行。截断的话，鼠标悬停这个动作
      // 就回答不了「这张是哪句话画的」—— 而那是它存在的唯一理由
      message: image.prompt,
      waitDuration: const Duration(milliseconds: 500),
      child: Material(
        color: scheme.surfaceContainerHigh,
        borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
        clipBehavior: Clip.antiAlias,
        child: InkWell(
          onTap: onTap,
          child: bytes.when(
            loading: () => const Center(
              child: SizedBox(
                width: 16,
                height: 16,
                child: CircularProgressIndicator(strokeWidth: 2),
              ),
            ),
            // 取不到就说取不到，**不画一个空格子** —— 空格子读起来像
            // 「这张图是空白的」，而事实是它没下下来
            error: (e, _) => Center(
              child: Icon(
                Icons.broken_image_outlined,
                size: 18,
                color: scheme.onSurfaceVariant,
              ),
            ),
            data: (b) => Image.memory(
              b,
              fit: BoxFit.cover,
              width: double.infinity,
              height: double.infinity,
              // 按显示尺寸解码，不按原图：一格 180 宽的位置放一张 1024×1024
              // 的 png，全分辨率解出来在光栅缓存里是 4 MB —— 一屏二十格
              // 就是 80 MB
              cacheWidth: 400,
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
