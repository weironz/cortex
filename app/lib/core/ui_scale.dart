/// 界面缩放与聊天文字大小的档位、解析与组合。
///
/// # 为什么是两个旋钮
///
/// 「字太小」有两种诉求，一个旋钮表达不了：
///
/// * **整体都小** —— 高分屏上 11px 的次要信息读不动。这是**全局**的事，
///   对话、侧栏、设置页一起放大；
/// * **只有对话读着累** —— 界面是扫的，对话是读的。长回答要凑近看，
///   而把侧栏一起放大只会让一屏装的东西更少。
///
/// 聊天档是**乘在界面档之上**的（参考 Hermes 的同名设置）：最终聊天区的
/// 缩放 = 界面缩放 × 聊天缩放，其余界面只吃界面缩放。
///
/// # 为什么 Flutter 里只用 textScaler
///
/// 桌面端没有浏览器那种「整页 zoom」；`TextScaler` 改写的是**每一个 Text
/// 的字号**，效果最接近。图标与固定尺寸的控件不跟着长（它们是形状不是
/// 文字），个别装文字的固定高度容器在档位档自己改 —— 见 `panel_header`
/// 的 minHeight。
library;

import 'package:flutter/material.dart';

/// 可选档位。两头都给：嫌字大与嫌字小的诉求都真实存在。
const List<double> kScalePresets = <double>[0.9, 1.0, 1.1, 1.25, 1.5, 1.75];

/// 默认档。**就是 1.0** —— 加这个设置之前长什么样，默认还长什么样。
const double kDefaultScale = 1.0;

/// 界面缩放与聊天文字大小的一对值。都是档位之一（见 [kScalePresets]）。
class UiScale {
  const UiScale({this.ui = kDefaultScale, this.chat = kDefaultScale});

  /// 全局档：改写整棵树的 textScaler。
  final double ui;

  /// 聊天档：**乘在全局之上**、只作用于对话区与输入框。
  final double chat;

  @override
  bool operator ==(Object other) =>
      other is UiScale && other.ui == ui && other.chat == chat;

  @override
  int get hashCode => Object.hash(ui, chat);

  @override
  String toString() => 'UiScale(ui: $ui, chat: $chat)';
}

/// 解析落盘的缩放值。
///
/// * 没存过、文件坏了、不是数、非正数 → `null`，调用方落回 [kDefaultScale]；
/// * 认得出的**吸附到最近的档位** —— 设置页只提供档位，存一个档位之外的
///   值会让选择卡片没有一个亮着，而「亮着哪一个」正是这一页唯一的可见状态。
double? parseScale(Object? raw) {
  final v = switch (raw) {
    final num n => n.toDouble(),
    final String s => double.tryParse(s.trim()),
    _ => null,
  };
  if (v == null || v.isNaN || v.isInfinite || v <= 0) return null;
  var best = kScalePresets.first;
  for (final p in kScalePresets) {
    if ((v - p).abs() < (v - best).abs()) best = p;
  }
  return best;
}

/// 把我们的档位组合进**当前生效**的 textScaler。
///
/// [base] 从 `MediaQuery.textScalerOf(context)` 取（`scale(16) / 16` 得到它
/// 在 16px 上的有效系数）—— 平台自己的文字缩放偏好（Android 的字体大小、
/// macOS 的辅助功能）在这里被**保留**，而不是被我们的档位整个顶掉。
TextScaler composeTextScaler(double base, double factor) =>
    TextScaler.linear(base * factor);

/// 从一个已生效的 textScaler 里取出有效系数（在 16px 上采样）。
double scaleFactorOf(TextScaler scaler) => scaler.scale(16) / 16;
