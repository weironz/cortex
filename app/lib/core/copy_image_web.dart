/// 浏览器上的「复制图片」—— `navigator.clipboard.write` + 一个 `Blob`。
///
/// # 只能放 PNG
///
/// 浏览器对剪贴板里放什么类型管得很死：Chromium 系只允许 `image/png`
/// （放别的会抛 `NotAllowedError`）。所以非 PNG 的要先重编 —— 走
/// `dart:ui` 解码再编成 PNG，与 Windows 那侧解码同一条路。
///
/// # 为什么可能失败
///
/// 剪贴板写入要求**页面处于焦点**且**由用户手势触发**。从按钮点进来
/// 两个条件都成立，但 Firefox 至今没实现 `ClipboardItem`（写入直接抛）。
/// 所以这条路一定要能回 false，让界面说一句话。
library;

import 'dart:async';
import 'dart:js_interop';
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:web/web.dart' as web;

Future<bool> copyImageToClipboard(Uint8List bytes) async {
  final png = await _asPng(bytes);
  if (png == null) return false;
  try {
    final blob = web.Blob(
      [png.toJS].toJS,
      web.BlobPropertyBag(type: 'image/png'),
    );
    final item = web.ClipboardItem({'image/png': blob}.jsify()! as JSObject);
    await web.window.navigator.clipboard.write([item].toJS).toDart;
    return true;
  } on Object {
    // Firefox 没有 ClipboardItem、页面失焦、用户拒了权限 —— 都落这儿。
    // 分不清也**不该**假装分得清：界面统一说「没能放进剪贴板」
    return false;
  }
}

/// 已经是 PNG 就原样用；不是就重编一遍。
///
/// 判据是**字节头**，不是我们记着的那个 mime：mime 来自服务端嗅探，
/// 而这里错一次的代价是浏览器直接拒收，用户只看到「复制失败」。
Future<Uint8List?> _asPng(Uint8List bytes) async {
  const magic = [0x89, 0x50, 0x4E, 0x47];
  if (bytes.length >= 4 &&
      bytes[0] == magic[0] &&
      bytes[1] == magic[1] &&
      bytes[2] == magic[2] &&
      bytes[3] == magic[3]) {
    return bytes;
  }
  try {
    final codec = await ui.instantiateImageCodec(bytes);
    final image = (await codec.getNextFrame()).image;
    try {
      final data = await image.toByteData(format: ui.ImageByteFormat.png);
      return data?.buffer.asUint8List();
    } finally {
      image.dispose();
    }
  } on Object {
    return null;
  }
}
