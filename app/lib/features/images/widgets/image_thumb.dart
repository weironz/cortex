/// 画廊里的一格。
library;

import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../../core/theme.dart';
import '../../../models/generated_image.dart';
import '../../../state/blob_bytes.dart';
import 'image_actions.dart';

class ImageThumb extends ConsumerWidget {
  const ImageThumb({
    super.key,
    required this.image,
    required this.onTap,
    this.onSaid,
    this.onChanged,
    this.selected = false,
    this.selecting = false,
    this.onToggleSelect,
  });

  final GeneratedImage image;
  final VoidCallback onTap;

  /// 这一格被勾中了。
  final bool selected;

  /// 整面墙都在多选态 —— 这时**点一下就是勾选**，不是打开。
  ///
  /// 不这么做的话，用户勾了三张之后想勾第四张，得记得按住 Ctrl，
  /// 而漏按的表现是弹出一张大图、勾选还在（他会以为是误触）。
  final bool selecting;

  /// `null` = 这一格不参与多选（对话里那张）。
  final VoidCallback? onToggleSelect;

  /// 动作做完之后要说的那句话（页面拿它弹 SnackBar）。
  final void Function(String message)? onSaid;

  /// 图库内容或分享状态变了。
  final VoidCallback? onChanged;

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
        clipBehavior: Clip.antiAlias,
        // 勾中的画一圈实线边框 + 一个角标。**不是只改底色** ——
        // 缩略图铺满整格，底色一点都露不出来。
        //
        // ⚠️ 圆角走 `shape` 而不是 `borderRadius`：`Material` 断言两者
        // 不能同时给，而同时给的表现是**整页红屏**
        shape: RoundedRectangleBorder(
          // 卡片档圆角（radiusCard）—— 与项目卡、智能体卡同一个数
          borderRadius: BorderRadius.circular(CortexTokens.radiusCard),
          side: selected
              ? BorderSide(color: scheme.primary, width: 2.5)
              : BorderSide.none,
        ),
        child: InkWell(
          onTap: selecting && onToggleSelect != null ? onToggleSelect : onTap,
          // 长按进多选（触屏）；Ctrl / ⌘ 点也行，见 `_ImagePageState`
          onLongPress: onToggleSelect,
          // 右键菜单与对话里那张**共用同一份动作**（`ImageActions`）——
          // 两处各写一遍的下场是「在对话里能复制链接，在图库里不能」，
          // 而用户根本分不清那是两个功能
          onSecondaryTapDown: (d) => showImageContextMenu(
            context,
            d.globalPosition,
            ImageActions(
              ref: ref,
              hash: image.hash,
              galleryId: image.id,
              prompt: image.prompt,
              shareUrl: image.shareUrl,
              said: onSaid ?? (_) {},
              onRemoved: onChanged,
            ),
          ),
          child: Stack(
            fit: StackFit.expand,
            children: [
              _picture(context, ref, bytes),
              // 多选态下**每一格**都要有勾选框，不只是勾中的那些 ——
              // 只画勾中的，用户看不出别的格子也能勾
              if (selecting || selected)
                Positioned(
                  top: 6,
                  left: 6,
                  child: Container(
                    decoration: BoxDecoration(
                      color: selected ? scheme.primary : scheme.surface,
                      shape: BoxShape.circle,
                      border: Border.all(color: scheme.outlineVariant),
                    ),
                    padding: const EdgeInsets.all(2),
                    child: Icon(
                      selected ? Icons.check_rounded : Icons.circle_outlined,
                      size: 13,
                      color: selected ? scheme.onPrimary : scheme.outline,
                    ),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }

  Widget _picture(
    BuildContext context,
    WidgetRef ref,
    AsyncValue<Uint8List> bytes,
  ) {
    final scheme = Theme.of(context).colorScheme;
    return bytes.when(
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
      data: (b) => _InkPicture(bytes: b),
    );
  }
}

/// 图本体 —— 画在 ink 层上，不画在子树里。
///
/// # ⚠️ 为什么不是 `Image.memory`
///
/// `Image.memory` 是 Material **子树里**的一个不透明矩形，而 `InkWell`
/// 的水波纹画在 Material 的 ink 层上 —— 在子树**底下**。图铺满整格之后
/// 波纹被整张盖住，hover / 点按看起来完全没反应，像一格死图。
/// `Ink.image` 把图挪进 ink 层本身：装饰先画、后来的波纹叠在图上面。
///
/// 拆成 StatefulWidget 只为一件事：`Ink.image` 没有 `errorBuilder`
/// （只有 `onImageError` 回调），解码失败的兜底图标得自己记一位。
class _InkPicture extends StatefulWidget {
  const _InkPicture({required this.bytes});

  final Uint8List bytes;

  @override
  State<_InkPicture> createState() => _InkPictureState();
}

class _InkPictureState extends State<_InkPicture> {
  /// 字节到手了但**解不出来**（坏 blob）。取不到字节的情况在外面的
  /// `bytes.when(error:)` 里，走不到这儿。
  bool _broken = false;

  @override
  void didUpdateWidget(_InkPicture oldWidget) {
    super.didUpdateWidget(oldWidget);
    // 刷新可能换来一份新字节 —— 上一份坏不代表这一份也坏
    if (!identical(oldWidget.bytes, widget.bytes)) _broken = false;
  }

  @override
  Widget build(BuildContext context) {
    if (_broken) {
      // 与「取不到」的图标一致：对用户来说都是「这张图坏了」，
      // 分成两种画法只会让人以为是两种问题
      return Center(
        child: Icon(
          Icons.broken_image_outlined,
          size: 18,
          color: Theme.of(context).colorScheme.onSurfaceVariant,
        ),
      );
    }
    return LayoutBuilder(
      builder: (context, c) => Ink.image(
        // ⚠️ **按这一格真实的物理像素解码，不按原图，也不写死一个数。**
        //
        // 一张 1024×1024 的 png 全分辨率解出来在光栅缓存里是 4 MB，
        // 一屏二十格就是 80 MB。
        //
        // 从前这里是写死的 400。格子从 180 放宽到最多 330 之后，在 2×
        // 屏上就不够了 —— 表现是缩略图发糊，而没有任何报错。乘上
        // `devicePixelRatio` 才是「这一格真正需要多少像素」。
        //
        // `ResizeImage(width:)` 与 `Image.memory` 的 `cacheWidth` 是同一条
        // 路 —— 后者内部就是 `ResizeImage.resizeIfNeeded` 包一层，逐字等价
        image: ResizeImage(
          MemoryImage(widget.bytes),
          width: decodeWidthFor(context, c.maxWidth),
        ),
        fit: BoxFit.cover,
        onImageError: (_, _) {
          if (mounted) setState(() => _broken = true);
        },
      ),
    );
  }
}

/// 这一格真正需要多少物理像素。
///
/// 夹在 [1, 1400]：无界约束下 `maxWidth` 是 `infinity`（取整之后是个垃圾
/// 值，解码器当场抛），而上限挡住「有人把一格拉到整屏宽」时把原图整张
/// 解出来。
int decodeWidthFor(BuildContext context, double logicalWidth) {
  final dpr = MediaQuery.maybeDevicePixelRatioOf(context) ?? 1.0;
  final px = logicalWidth * dpr;
  if (!px.isFinite || px <= 0) return 512;
  return px.round().clamp(1, 1400);
}
