library;

import 'dart:convert';
import 'dart:io';

/// 桌面端：一个纯文本 JSON，放在与本地 agent 同一个状态目录下。
///
/// 不加密、也不需要 —— 里面只有一个地址。与 outbox / workspaces.json
/// 同一个目录，删掉整个目录就是「恢复出厂」，不用记两个地方。
File _file() {
  final base = Platform.isWindows
      ? Platform.environment['LOCALAPPDATA']
      : (Platform.environment['XDG_DATA_HOME'] ??
            (Platform.environment['HOME'] == null
                ? null
                : '${Platform.environment['HOME']}/.local/share'));
  final dir = Directory('${base ?? Directory.systemTemp.path}'
      '${Platform.pathSeparator}cortex');
  if (!dir.existsSync()) dir.createSync(recursive: true);
  return File('${dir.path}${Platform.pathSeparator}settings.json');
}

Future<Map<String, String>> readSettings() async {
  try {
    final f = _file();
    if (!f.existsSync()) return const {};
    final raw = jsonDecode(await f.readAsString()) as Map<String, dynamic>;
    return raw.map((k, v) => MapEntry(k, '$v'));
  } on Object catch (_) {
    // 文件坏了 / 读不了 —— 都只意味着「用默认值」，没有一种该让应用起不来
    return const {};
  }
}

Future<void> writeSettings(Map<String, String> values) async {
  try {
    await _file().writeAsString(jsonEncode(values));
  } on Object catch (_) {
    // 存不下只影响下次启动要重填一次。为它打断当前操作不值得
  }
}
