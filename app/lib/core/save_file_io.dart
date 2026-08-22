import 'dart:io';
import 'dart:typed_data';

import 'package:file_picker/file_picker.dart';

/// 弹系统的「另存为」，返回真实落盘路径；用户取消回 `null`。
///
/// # ⚠️ 从前它是静默往 `~/Downloads` 里写
///
/// 没有对话框、没有回路径，界面上只弹一句「存好了」。2026-08-22 用户报的
/// 就是这个：**「并不会真正下载到本地」** —— 文件其实写进去了，但他既没
/// 选过位置、也不知道去哪儿找，所以那句「存好了」在他看来就是假的。
///
/// 「说存好了却说不出存在哪」比存不下来更糟：后者他会重试，前者他会去
/// 翻半天文件夹。所以返回值从 `bool` 改成了**路径**。
///
/// # ⚠️ `saveFile` 在桌面端**只回路径，不写文件**
///
/// 它的 `bytes` 参数是给移动端/Web 的。装着的 11.0.3 在 Windows 上拿到
/// 路径就返回了 —— 只传 `bytes` 而不自己写的话，对话框弹得好好的、
/// 函数回一个像样的路径，而那个位置上**一个文件都没有**。
///（pub.dev 上更新版本的文档写着它会自己写，与装着的这一版不符。）
Future<String?> saveBytesAs(
  Uint8List bytes,
  String filename, {
  String mimeType = 'application/octet-stream',
}) async {
  final path = await FilePicker.saveFile(
    dialogTitle: '另存为',
    fileName: filename,
    // 传着不亏：移动端/Web 的实现会用它。桌面端忽略，所以下面还要自己写
    bytes: bytes,
  );
  if (path == null) return null;
  // **自己写一遍。** 桌面端上面那一步只是选了个位置
  await File(path).writeAsBytes(bytes, flush: true);
  return path;
}
