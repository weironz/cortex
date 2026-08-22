import 'dart:js_interop';
import 'dart:typed_data';

import 'package:web/web.dart' as web;

/// 走浏览器的下载：造一个 Blob，点一次隐形的 `<a download>`，再释放 URL。
///
/// **必须 `revokeObjectURL`** —— 不释放的话每下载一次就漏一份完整字节，
/// 而那在一个开着好几天的标签页里是真的会吃光内存的。
///
/// 恒返回文件名（而不是路径）：**浏览器不告诉我们它落在哪儿**，也不告诉
/// 我们用户在「另存为」对话框里点了什么。
///
/// 桌面那侧回的是真实路径（`FilePicker.saveFile`），所以调用方拿到的这个
/// 字符串在两个平台上含义不同。界面上因此说的是「已下载：a.png」而不是
/// 「存到了 …」—— 编一个路径出来是这条路上最容易撒的谎。
Future<String?> saveBytesAs(
  Uint8List bytes,
  String filename, {
  String mimeType = 'application/octet-stream',
}) async {
  final blob = web.Blob([bytes.toJS].toJS, web.BlobPropertyBag(type: mimeType));
  final url = web.URL.createObjectURL(blob);
  final anchor = web.document.createElement('a') as web.HTMLAnchorElement
    ..href = url
    ..download = filename;
  anchor.click();
  web.URL.revokeObjectURL(url);
  return filename;
}
