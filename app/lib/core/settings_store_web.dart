library;

import 'dart:convert';

import 'package:web/web.dart' as web;

/// Web：`localStorage`。
///
/// 与凭据用 `sessionStorage` 不同 —— 那是刻意的：凭据的暴露要**按时间**
/// 限住（关掉标签页就没了），而地址不是秘密，跨会话记住才有意义。
const String _key = 'cortex.settings';

Future<Map<String, String>> readSettings() async {
  try {
    final raw = web.window.localStorage.getItem(_key);
    if (raw == null || raw.isEmpty) return const {};
    return (jsonDecode(raw) as Map<String, dynamic>)
        .map((k, v) => MapEntry(k, '$v'));
  } on Object catch (_) {
    return const {};
  }
}

Future<void> writeSettings(Map<String, String> values) async {
  try {
    web.window.localStorage.setItem(_key, jsonEncode(values));
  } on Object catch (_) {
    // 隐私模式下 localStorage 会抛。降级成「不记住」，不是崩溃
  }
}
