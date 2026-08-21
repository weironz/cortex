import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'app.dart';
import 'core/single_instance.dart';

void main() {
  // **第一行就是守门**，在任何状态被碰到之前。两份桌面端同时跑的后果
  // 不是「多一个窗口」：它们共用 %LOCALAPPDATA%\cortex，会互相作废对方
  // 的 refresh token、互相覆盖对方存的地址 —— 2026-08-21 的连锁事故
  // 就是这么起的头。见 core/single_instance.dart。
  if (!ensureSingleInstance()) {
    // **`return` 一个人不够。** 窗口的消息循环在 C++ runner 里，不由
    // 这个 `main()` 驱动 —— 只 return 的话进程活着、窗口开着，
    // 只是永远白屏。见 `exitDuplicateInstance` 的注释
    exitDuplicateInstance();
    return;
  }
  runApp(const ProviderScope(child: CortexApp()));
}
