import 'dart:io';
import 'dart:typed_data';

/// 写进用户的下载目录，返回是否真的写成了。
///
/// 桌面端目前走不到这里（唯一的调用方 `SandboxDownloadButton` 在
/// `kLocalAgentSupported` 时不显示）。**仍然写成真的能用**：一个
/// 「类型对得上但一调就抛」的实现，会在将来某天有人复用它时才炸，
/// 而那时没人记得这里是个占位。
Future<bool> saveBytesAs(Uint8List bytes, String filename) async {
  final home =
      Platform.environment['USERPROFILE'] ?? Platform.environment['HOME'];
  final dir = home == null
      ? Directory.systemTemp
      : Directory('$home${Platform.pathSeparator}Downloads');
  final target = Directory(
    await dir.exists() ? dir.path : Directory.systemTemp.path,
  );
  final file = File('${target.path}${Platform.pathSeparator}$filename');
  await file.writeAsBytes(bytes, flush: true);
  return true;
}
