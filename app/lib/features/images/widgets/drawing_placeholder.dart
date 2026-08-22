/// 「正在创建图片」—— 一块缓慢明灭的点阵。
///
/// # 为什么不是一个转圈图标
///
/// 生图要几十秒。一个通用的 spinner 在第二十秒读起来就是「卡住了」，
/// 而且它占的是一行，用户完全看不出「这里将来会出现一张图」。
///
/// 点阵占的是**那张图将来的位置**（方的、和缩略图一样大），所以它同时说了
/// 三件事：在干活、要等一会儿、结果会长在这儿。ChatGPT 用的是同一个形状。
///
/// # 为什么不用图片资源
///
/// 一张动图要打包、要两套主题、要按 DPR 出几份。`CustomPainter` 画几十个
/// 点是零资源、跟着主题走、任意尺寸都清楚。
library;

import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../../../core/motion.dart';
import '../../../core/theme.dart';

class DrawingPlaceholder extends StatefulWidget {
  const DrawingPlaceholder({super.key, this.size = 240});

  final double size;

  @override
  State<DrawingPlaceholder> createState() => _DrawingPlaceholderState();
}

class _DrawingPlaceholderState extends State<DrawingPlaceholder>
    with SingleTickerProviderStateMixin {
  late final AnimationController _c = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 2600),
  );

  @override
  void initState() {
    super.initState();
    // ⚠️ **在这里不能读 MotionPref**（`initState` 里没有 `InheritedWidget`）。
    // 由 `didChangeDependencies` 决定跑不跑，见那里
    _c.repeat();
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    // 「减少动效」时**停在一帧上**，而不是让它转。
    //
    // 不是把动画调慢 —— 一个慢下来的动画仍然是动画。停下来之后这块点阵
    // 还在（位置、大小、「正在创建图片」那行字都在），少的只有闪烁。
    if (motionDuration(context, const Duration(milliseconds: 1)) ==
        Duration.zero) {
      if (_c.isAnimating) _c.stop();
    } else if (!_c.isAnimating) {
      _c.repeat();
    }
  }

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        Container(
          width: widget.size,
          height: widget.size,
          decoration: BoxDecoration(
            color: theme.colorScheme.surfaceContainerHigh,
            borderRadius: BorderRadius.circular(CortexTokens.radiusLg),
          ),
          child: Stack(
            children: [
              Positioned.fill(
                child: AnimatedBuilder(
                  animation: _c,
                  builder: (_, _) => CustomPaint(
                    painter: _DotsPainter(
                      t: _c.value,
                      color: theme.cortex.foregroundTertiary,
                    ),
                  ),
                ),
              ),
              Positioned(
                left: 14,
                top: 12,
                child: Text(
                  '正在创建图片',
                  style: theme.textTheme.labelSmall?.copyWith(
                    color: theme.cortex.foregroundTertiary,
                  ),
                ),
              ),
            ],
          ),
        ),
      ],
    );
  }
}

/// 一片按相位错开明灭的点。
///
/// 每个点的相位由它的坐标决定（而不是随机数）：随机的话每次重建都是一副
/// 新样子，看起来像画面在抖；坐标决定的相位是稳定的，重建之后接着走。
class _DotsPainter extends CustomPainter {
  const _DotsPainter({required this.t, required this.color});

  /// 0..1 的循环相位。
  final double t;
  final Color color;

  @override
  void paint(Canvas canvas, Size size) {
    const gap = 14.0;
    const radius = 1.6;
    final paint = Paint()..style = PaintingStyle.fill;
    final cols = (size.width / gap).floor();
    final rows = (size.height / gap).floor();
    // 从中心往外扩散：一行行推过去看起来像扫描，而扩散更像「正在成形」
    final cx = (cols - 1) / 2;
    final cy = (rows - 1) / 2;
    for (var x = 0; x < cols; x++) {
      for (var y = 0; y < rows; y++) {
        final d = math.sqrt(math.pow(x - cx, 2) + math.pow(y - cy, 2));
        final phase = (t - d * 0.045) % 1.0;
        // 一条平滑的驼峰：`sin` 在 0..1 上走半个周期，两端都收到 0，
        // 于是循环处不会有一道明显的接缝
        final wave = math.sin(phase * math.pi);
        paint.color = color.withValues(alpha: 0.10 + 0.35 * wave * wave);
        canvas.drawCircle(
          Offset(gap / 2 + x * gap, gap / 2 + y * gap),
          radius,
          paint,
        );
      }
    }
  }

  @override
  bool shouldRepaint(_DotsPainter old) => old.t != t || old.color != color;
}
