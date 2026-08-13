import 'dart:js_interop';
import 'dart:typed_data';

import 'package:web/web.dart' as web;

/// 走浏览器的下载：造一个 Blob，点一次隐形的 `<a download>`，再释放 URL。
///
/// **必须 `revokeObjectURL`** —— 不释放的话每下载一次就漏一份完整字节，
/// 而那在一个开着好几天的标签页里是真的会吃光内存的。
///
/// 恒返回 true：浏览器不告诉我们用户在「另存为」对话框里点了什么。
Future<bool> saveBytesAs(Uint8List bytes, String filename) async {
  final blob = web.Blob(
    [bytes.toJS].toJS,
    web.BlobPropertyBag(type: 'application/octet-stream'),
  );
  final url = web.URL.createObjectURL(blob);
  final anchor = web.document.createElement('a') as web.HTMLAnchorElement
    ..href = url
    ..download = filename;
  anchor.click();
  web.URL.revokeObjectURL(url);
  return true;
}
