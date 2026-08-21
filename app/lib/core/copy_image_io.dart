/// Windows 上的「复制图片」—— 往剪贴板放一份 `CF_DIB`。
///
/// # 为什么是 DIB 而不是 PNG
///
/// Windows 剪贴板的**通用**图片格式是 `CF_DIB`（一份没有文件头的 BMP）。
/// 画图、Word、Excel、绝大多数聊天软件认的都是它。PNG 在剪贴板上是一个
/// 「注册格式」，认它的程序少得多 —— 复制成功但粘不出来，是这条路最坏的
/// 结果，因为用户会以为是接收方的问题。
///
/// # 透明先合成到白底
///
/// `CF_DIB` 32 位在规范里是 `BI_RGB`，第四个字节写着「保留」。实际接收方
/// 各行其是：有的当 alpha，有的当 0（于是整张图变黑）。带透明的图直接放
/// 进去，粘到画图里就是一块黑。
///
/// 合成白底是**有损**的（透明信息没了），但它换来的是「粘出来就是你看到
/// 的那张」。要原样保留透明的人走「下载」那条 —— 那条一个字节都不改。
library;

import 'dart:async';
import 'dart:ffi';
import 'dart:io';
import 'dart:typed_data';
import 'dart:ui' as ui;

/// `CF_DIB`。见 winuser.h。
const int _cfDib = 8;

/// `GMEM_MOVEABLE`。剪贴板要的就是可移动块 —— 传固定块时
/// `SetClipboardData` 会失败。
const int _gmemMoveable = 0x0002;

/// 把这张图放进系统剪贴板。返回是否真的放进去了。
Future<bool> copyImageToClipboard(Uint8List bytes) async {
  if (!Platform.isWindows) return false;

  final ({int width, int height, Uint8List bgra})? raw = await _decodeBgra(
    bytes,
  );
  if (raw == null) return false;

  final dib = buildDib(raw.width, raw.height, raw.bgra);

  final user32 = DynamicLibrary.open('user32.dll');
  final kernel32 = DynamicLibrary.open('kernel32.dll');

  final openClipboard = user32
      .lookupFunction<Int32 Function(IntPtr), int Function(int)>(
        'OpenClipboard',
      );
  final emptyClipboard = user32
      .lookupFunction<Int32 Function(), int Function()>('EmptyClipboard');
  final setClipboardData = user32
      .lookupFunction<IntPtr Function(Uint32, IntPtr), int Function(int, int)>(
        'SetClipboardData',
      );
  final closeClipboard = user32
      .lookupFunction<Int32 Function(), int Function()>('CloseClipboard');
  final globalAlloc = kernel32
      .lookupFunction<IntPtr Function(Uint32, IntPtr), int Function(int, int)>(
        'GlobalAlloc',
      );
  final globalLock = kernel32
      .lookupFunction<
        Pointer<Uint8> Function(IntPtr),
        Pointer<Uint8> Function(int)
      >('GlobalLock');
  final globalUnlock = kernel32
      .lookupFunction<Int32 Function(IntPtr), int Function(int)>(
        'GlobalUnlock',
      );
  final globalFree = kernel32
      .lookupFunction<IntPtr Function(IntPtr), int Function(int)>('GlobalFree');

  final handle = globalAlloc(_gmemMoveable, dib.length);
  if (handle == 0) return false;

  final ptr = globalLock(handle);
  if (ptr == nullptr) {
    globalFree(handle);
    return false;
  }
  ptr.asTypedList(dib.length).setAll(0, dib);
  globalUnlock(handle);

  // 剪贴板一次只能被一个进程打开。别的程序正拿着时这里会失败 ——
  // 那是**正常的临时状态**，回 false 让界面说一句「再试一次」
  if (openClipboard(0) == 0) {
    globalFree(handle);
    return false;
  }
  emptyClipboard();
  final ok = setClipboardData(_cfDib, handle) != 0;
  closeClipboard();

  // ⚠️ 成功之后**不能** GlobalFree：那块内存的所有权已经交给系统了，
  // 释放它的表现是别人粘贴时读到一块已经还回去的内存
  if (!ok) globalFree(handle);
  return ok;
}

/// 解成「自下而上的 BGRA」—— DIB 要的那个形状。
Future<({int width, int height, Uint8List bgra})?> _decodeBgra(
  Uint8List bytes,
) async {
  ui.Image image;
  try {
    final codec = await ui.instantiateImageCodec(bytes);
    image = (await codec.getNextFrame()).image;
  } on Object {
    return null;
  }
  try {
    final data = await image.toByteData(format: ui.ImageByteFormat.rawRgba);
    if (data == null) return null;
    return (
      width: image.width,
      height: image.height,
      bgra: rgbaToBottomUpBgra(
        data.buffer.asUint8List(),
        image.width,
        image.height,
      ),
    );
  } finally {
    image.dispose();
  }
}

/// RGBA（自上而下、直通 alpha）→ BGRA（自下而上、合成白底）。
///
/// 抽出来是为了**能测**：DIB 里最容易错的两件事（行序反了、通道顺序反了）
/// 在界面上的表现分别是「图上下颠倒」与「红蓝互换」，而两者都要粘到别的
/// 程序里才看得见 —— 那种 bug 没有测试就只能靠人每次手验。
Uint8List rgbaToBottomUpBgra(Uint8List rgba, int width, int height) {
  final out = Uint8List(width * height * 4);
  final stride = width * 4;
  for (var y = 0; y < height; y++) {
    // 自下而上：源的最后一行是目标的第一行
    final src = (height - 1 - y) * stride;
    final dst = y * stride;
    for (var x = 0; x < stride; x += 4) {
      final r = rgba[src + x];
      final g = rgba[src + x + 1];
      final b = rgba[src + x + 2];
      final a = rgba[src + x + 3];
      // 合成白底。a == 255 时下面这三行等价于直接搬运，所以不必特判
      out[dst + x] = _overWhite(b, a);
      out[dst + x + 1] = _overWhite(g, a);
      out[dst + x + 2] = _overWhite(r, a);
      // 第四个字节：写 255。规范说它保留，而把 alpha 原样写进去的接收方
      // 会把半透明像素画成黑的 —— 我们已经把颜色合成过了，这里再声明
      // 一次「不透明」才自洽
      out[dst + x + 3] = 0xFF;
    }
  }
  return out;
}

/// 一个通道合成到白底上。
int _overWhite(int channel, int alpha) =>
    alpha == 255 ? channel : ((channel * alpha + 255 * (255 - alpha)) ~/ 255);

/// `BITMAPINFOHEADER` + 像素。**不带** BMP 文件头（`BM` 那两个字节）——
/// `CF_DIB` 要的就是从信息头开始的那一段，多两个字节就认不出来了。
Uint8List buildDib(int width, int height, Uint8List bottomUpBgra) {
  const headerSize = 40;
  final out = Uint8List(headerSize + bottomUpBgra.length);
  final h = ByteData.sublistView(out, 0, headerSize);
  h.setUint32(0, headerSize, Endian.little); // biSize
  h.setInt32(4, width, Endian.little); // biWidth
  // 正数 = 自下而上。负数是自上而下，但**不少老程序不认负高度** ——
  // 表现是粘出来上下颠倒或者干脆是一片噪点
  h.setInt32(8, height, Endian.little); // biHeight
  h.setUint16(12, 1, Endian.little); // biPlanes
  h.setUint16(14, 32, Endian.little); // biBitCount
  h.setUint32(16, 0, Endian.little); // biCompression = BI_RGB
  h.setUint32(20, bottomUpBgra.length, Endian.little); // biSizeImage
  h.setInt32(24, 0, Endian.little); // biXPelsPerMeter
  h.setInt32(28, 0, Endian.little); // biYPelsPerMeter
  h.setUint32(32, 0, Endian.little); // biClrUsed
  h.setUint32(36, 0, Endian.little); // biClrImportant
  out.setAll(headerSize, bottomUpBgra);
  return out;
}
