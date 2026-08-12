library;

import 'dart:convert';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import '../core/local_llm.dart';

/// 桌面端能存 —— 而且是存进系统凭据库，不是明文文件。
const bool kCanStoreLocalLlm = true;

const _storage = FlutterSecureStorage();

/// 带应用前缀：系统凭据库是**全机共享**的命名空间。
const String _key = 'cortex.local_llm';

/// 读回本机 LLM 配置。读不出来一律当作「没配过」。
///
/// 钥匙串锁着、libsecret 没跑、条目损坏 —— 都落在这里。没有一种该让
/// 设置页打不开，它们的意思都只是「你得重新填一次」。
Future<LocalLlmConfig> readLocalLlm() async {
  try {
    final raw = await _storage.read(key: _key);
    if (raw == null || raw.isEmpty) return LocalLlmConfig.empty;
    return LocalLlmConfig.fromJson(jsonDecode(raw) as Map<String, dynamic>);
  } on Object catch (_) {
    return LocalLlmConfig.empty;
  }
}

/// 写入。
///
/// # Errors
/// 存不进去时抛 —— 与 token 那侧的「只打印一行」不同：这是用户**刚刚
/// 主动点了保存**的东西，静默失败会让他以为配好了，然后在离线模式里
/// 撞上一句「模型起不来」而完全不知道为什么。
Future<void> writeLocalLlm(LocalLlmConfig config) async {
  if (config.isEmpty) {
    await clearLocalLlm();
    return;
  }
  await _storage.write(key: _key, value: jsonEncode(config.toJson()));
}

Future<void> clearLocalLlm() async {
  try {
    await _storage.delete(key: _key);
  } on Object catch (_) {
    // 删一个本来就不在的键，或者够不着钥匙串。两种都没有后续动作
  }
}
