/// 「复制图片」在 Windows 上放进剪贴板的那份 DIB 字节。
///
/// # 为什么这一段非测不可
///
/// 它有两个**只有粘到别的程序里才看得见**的失败模式：
///
/// * 行序反了 → 粘出来上下颠倒
/// * 通道顺序反了 → 粘出来红蓝互换
///
/// 两者都不会抛异常、不会让复制失败，界面上一切正常。没有这条测试，
/// 它们只能靠人每次改完手动粘一次画图 —— 而那种验证迟早会被跳过。
library;

import 'dart:typed_data';

import 'package:cortex_app/core/copy_image_io.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('DIB 字节', () {
    test('信息头写的是 32 位 BI_RGB、正高度（自下而上）', () {
      final pixels = Uint8List(2 * 2 * 4);
      final dib = buildDib(2, 2, pixels);
      final h = ByteData.sublistView(dib, 0, 40);

      expect(h.getUint32(0, Endian.little), 40, reason: 'biSize 必须是 40');
      expect(h.getInt32(4, Endian.little), 2, reason: 'biWidth');
      expect(
        h.getInt32(8, Endian.little),
        2,
        reason:
            'biHeight 必须为正 —— 负数是自上而下，'
            '而不少老程序不认负高度，表现是粘出来上下颠倒或一片噪点',
      );
      expect(h.getUint16(14, Endian.little), 32, reason: 'biBitCount');
      expect(
        h.getUint32(16, Endian.little),
        0,
        reason: 'biCompression 必须是 BI_RGB(0)',
      );
      expect(
        h.getUint32(20, Endian.little),
        pixels.length,
        reason: 'biSizeImage',
      );
    });

    test('不带 BMP 文件头 —— CF_DIB 要的是从信息头开始那一段', () {
      final dib = buildDib(1, 1, Uint8List(4));
      expect(
        dib.sublist(0, 2),
        isNot([0x42, 0x4D]),
        reason: '多了 `BM` 那两个字节，接收方就认不出来了',
      );
      expect(dib.length, 40 + 4);
    });
  });

  group('像素转换', () {
    /// 一张 1×2 的图：上面一行纯红，下面一行纯蓝。
    ///
    /// 红蓝这一对是**故意选的** —— 通道顺序搞反时它俩会互换，
    /// 而绿色搞反了看不出来。
    Uint8List redOverBlue() => Uint8List.fromList([
      255, 0, 0, 255, // 第 0 行：红
      0, 0, 255, 255, // 第 1 行：蓝
    ]);

    test('行序翻过来了：源的最后一行变成目标的第一行', () {
      final out = rgbaToBottomUpBgra(redOverBlue(), 1, 2);
      // DIB 第一行 = 图像最底下那一行 = 蓝
      expect(
        out.sublist(0, 4),
        [255, 0, 0, 255],
        reason:
            'BGRA 里的蓝是 B=255 —— 这一行是源的最后一行（蓝），'
            '不翻的话粘出来上下颠倒',
      );
      // DIB 第二行 = 图像最上面那一行 = 红
      expect(out.sublist(4, 8), [
        0,
        0,
        255,
        255,
      ], reason: 'BGRA 里的红是 R=255（第三个字节）');
    });

    test('通道顺序是 BGRA，不是 RGBA', () {
      final out = rgbaToBottomUpBgra(
        Uint8List.fromList([255, 0, 0, 255]),
        1,
        1,
      );
      expect(
        out,
        [0, 0, 255, 255],
        reason:
            '一个纯红像素在 BGRA 里是 (0,0,255)。搞反的表现是'
            '粘出来红蓝互换，而复制本身不会报任何错',
      );
    });

    test('半透明合成到白底，且第四个字节声明不透明', () {
      // 50% 的黑，压在白底上应该得到中灰
      final out = rgbaToBottomUpBgra(Uint8List.fromList([0, 0, 0, 128]), 1, 1);
      expect(
        out[0],
        closeTo(127, 2),
        reason:
            'CF_DIB 的第四个字节在规范里是保留位，接收方各行其是 —— '
            '不合成的话，半透明像素在画图里粘出来是一块黑',
      );
      expect(out[3], 255, reason: '颜色已经合成过了，这里必须再声明一次不透明');
    });

    test('全不透明的像素颜色一个字节都不变', () {
      final out = rgbaToBottomUpBgra(
        Uint8List.fromList([10, 20, 30, 255]),
        1,
        1,
      );
      expect(
        out.sublist(0, 3),
        [30, 20, 10],
        reason:
            '合成公式在 a=255 时必须恰好等于直接搬运，'
            '差一个舍入就是整张图轻微变色',
      );
    });
  });
}
