/// 提示音的开关 —— 「通知」那一页管的东西。
///
/// # 为什么这一页只有声音，没有系统通知
///
/// 系统级通知（窗口最小化时弹一个横幅）要一个通知插件，而那会改动
/// Windows/macOS/Linux 三个 runner 的构建。**在它落地之前，这一页不
/// 声称「会通知你」** —— 那是 roadmap G 节里已经记着的话，也是
/// CLAUDE.md 约束 2 在界面上的样子：宁可少一个开关，不要一个打开
/// 也不发生任何事的开关。
///
/// 声音这一档不需要任何插件（`SystemSound` 在 flutter/services 里），
/// 所以它是**现在真的做得到**的那一半：窗口在后台但没最小化时，
/// 一声提示足够把人叫回来。
library;

import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'app_providers.dart';

class NotifyPrefs {
  const NotifyPrefs({this.onFinish = true, this.onConfirm = true});

  /// 一轮跑完、而你正在**别处**时响一声。
  ///
  /// 「在别处」是判据的一部分：盯着屏幕看它跑完的人不需要被提醒
  /// （那只会变成每轮一响的噪音）。
  final bool onFinish;

  /// 有一轮停下来等你确认时响一声。
  ///
  /// 默认开，且**比跑完那档更该开**：确认是唯一「不管就永远卡住」的状态。
  final bool onConfirm;

  NotifyPrefs copyWith({bool? onFinish, bool? onConfirm}) => NotifyPrefs(
    onFinish: onFinish ?? this.onFinish,
    onConfirm: onConfirm ?? this.onConfirm,
  );
}

class NotifyPrefsNotifier extends Notifier<NotifyPrefs> {
  static const String _finishKey = 'notify_sound_finish';
  static const String _confirmKey = 'notify_sound_confirm';

  @override
  NotifyPrefs build() {
    Future.microtask(_restore);
    return const NotifyPrefs();
  }

  Future<void> _restore() async {
    final saved = await ref.read(settingsReaderProvider)();
    if (!ref.mounted) return;
    // 认不出就按**开**处理：这两个开关的默认值是开，而一个读不懂的
    // 旧值不该把它静默关掉 —— 用户会以为提示音坏了
    state = NotifyPrefs(
      onFinish: saved[_finishKey] != 'off',
      onConfirm: saved[_confirmKey] != 'off',
    );
  }

  void setOnFinish(bool value) {
    state = state.copyWith(onFinish: value);
    ref.read(settingsPatcherProvider)(_finishKey, value ? 'on' : 'off');
  }

  void setOnConfirm(bool value) {
    state = state.copyWith(onConfirm: value);
    ref.read(settingsPatcherProvider)(_confirmKey, value ? 'on' : 'off');
  }
}

final notifyPrefsProvider = NotifierProvider<NotifyPrefsNotifier, NotifyPrefs>(
  NotifyPrefsNotifier.new,
);

/// 响一声。**吞掉异常** —— 没有音频设备（CI、服务器上跑的 Web、
/// 静音的机器）时它会抛，而一次提示音失败绝不该让那一轮对话失败。
Future<void> playAlert() async {
  try {
    await SystemSound.play(SystemSoundType.alert);
  } on Object {
    // 故意空：见上
  }
}
