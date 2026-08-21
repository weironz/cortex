import 'package:flutter/material.dart';

import '../../../core/theme.dart';

/// 一家供应商的品牌标。
///
/// # 为什么值得单独一个部件
///
/// 它出现在三个地方（来源列表、总览卡片墙、详情页头部），三处尺寸不同
/// 但**回落规则必须一样**。各画各的下场是：卡片墙上认得出 DeepSeek，
/// 列表里却是个字母 —— 同一家在同一屏里长出两副面孔。
///
/// # 找不到图不是异常，是常态
///
/// * 「自定义」那家**本来就没有品牌**（中转站、公司网关、自建 vLLM）；
/// * 服务端放行的供应商比我们发的多（`available()` 有 38 家，
///   `SHIPPED` 只发 11 家），知道自己在做什么的人可以直接指名一家长尾的；
/// * 品牌标是可以随时被撤掉的（见 NOTICE 里那段）。
///
/// 所以首字母圆标不是「等图片到位之前的临时方案」，它是这条路的另一半。
class ProviderMark extends StatelessWidget {
  const ProviderMark({
    super.key,
    required this.provider,
    required this.displayName,
    this.size = 20,
  });

  /// 供应商 id。**资源文件名就是它** —— `anthropic` ↔
  /// `assets/providers/anthropic.png`，中间没有一张要维护的映射表。
  final String provider;

  /// 找不到图时拿它取首字母。用显示名而不是 id：
  /// 「自定义」取到「自」，比取到 `c` 有用得多。
  final String displayName;

  final double size;

  @override
  Widget build(BuildContext context) {
    // 按屏幕像素密度解码，**不按文件原始尺寸**。
    //
    // 这些图来自两处：goose 取件的是 128×128，lobe-icons 那几个是 640×640。
    // 不限制的话后者每张解码成 640²×4B ≈ 1.6 MB，而它实际只画 20 逻辑像素 ——
    // 卡片墙上十张同屏就是几 MB 的纯浪费。
    //
    // 在这里解决而不是把文件缩一遍：以后再丢一个图标进来，
    // 不管它多大都不用有人记得先处理一道。
    final ratio = MediaQuery.maybeDevicePixelRatioOf(context) ?? 1.0;
    final decode = (size * ratio).round();

    return Image.asset(
      'assets/providers/$provider.png',
      width: size,
      height: size,
      cacheWidth: decode,
      cacheHeight: decode,
      // 图标是方的，不该被拉变形；给的框不方时留白而不是裁
      fit: BoxFit.contain,
      filterQuality: FilterQuality.medium,
      // ⚠️ **必须给 errorBuilder。** 缺资源时 `Image.asset` 抛的是
      // 一个渲染期异常，而那类异常不冒红 —— 症状是整块界面空白
      // （2026-08-21 的「外观」页就是这么发出去的）。
      errorBuilder: (context, _, _) =>
          _Monogram(provider: provider, displayName: displayName, size: size),
    );
  }
}

/// 没有品牌标时那个首字母圆标。
class _Monogram extends StatelessWidget {
  const _Monogram({
    required this.provider,
    required this.displayName,
    required this.size,
  });

  final String provider;
  final String displayName;
  final double size;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final text = displayName.trim().isEmpty ? provider : displayName.trim();
    final initial = text.isEmpty ? '?' : text.characters.first.toUpperCase();

    return Container(
      width: size,
      height: size,
      alignment: Alignment.center,
      decoration: BoxDecoration(
        // **中性**（规范第一节）：这是身份，不是动作。给每家分一个彩色
        // 圆标看着热闹，但一屏十几个彩点会把真正要注意的东西淹掉 ——
        // 而且随机配色迟早会撞上我们自己的反馈色，让人以为它在报状态
        color: scheme.surfaceContainerHighest,
        // **圆角方块，不是圆形**：旁边那些真品牌标都是方的，
        // 混一个圆的进去会读成「另一类东西」，而它要冒充的正是同一类
        borderRadius: BorderRadius.circular(CortexTokens.radiusSm),
        border: Border.all(color: scheme.outlineVariant),
      ),
      child: Text(
        initial,
        style: TextStyle(
          // 跟着框走，不写死：这个部件在三处用了三种尺寸
          fontSize: size * 0.48,
          fontWeight: FontWeight.w600,
          color: scheme.onSurfaceVariant,
          height: 1,
        ),
      ),
    );
  }
}
