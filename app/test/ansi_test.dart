import 'package:cortex_app/core/ansi.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

const _base = TextStyle(color: Color(0xFF111111));

String _text(String s) => parseAnsi(s, base: _base).map((x) => x.text).join();

void main() {
  group('ANSI', () {
    /// **正文一个字都不能丢。**
    ///
    /// 这是这个解析器唯一不能出错的地方：颜色错了只是难看，
    /// 而吃掉正文会让用户以为命令没输出那一段。
    test('颜色序列被吃掉，字一个不少', () {
      expect(_text('\x1B[32m通过\x1B[0m'), '通过');
      expect(_text('a\x1B[1;31mb\x1B[0mc'), 'abc');
      expect(_text('没有任何转义'), '没有任何转义');
    });

    /// 光标移动 / 清屏这类**对着字符网格**说的序列：认出来并丢掉。
    ///
    /// 照做既做不到（这里是只往下长的文本块），留着就是乱码。
    test('光标与清除序列丢掉，不漏成正文', () {
      expect(_text('\x1B[2J\x1B[H干净'), '干净');
      expect(_text('一\x1B[2A二\x1B[K三'), '一二三');
    });

    /// 输出被截断时，末尾常常是半个序列。
    /// 把它当正文吐出来是最难看的一种失败。
    test('截断的序列整段丢掉', () {
      expect(_text('好了\x1B[3'), '好了');
      // 落单的 ESC 当正文留着：一个不成形的序列丢掉的话，正文会莫名其妙
      // 少一段，而那比多一个不可见字符难查得多
      expect(_text('好了\x1B'), '好了\x1B');
    });

    /// `38;5;n` / `38;2;r;g;b` 的参数**必须被吃掉**。
    ///
    /// 不吃的话那个 n 会被当成下一个 SGR 码 —— `38;5;1` 变成
    /// 「重置 + 红」，颜色错、时机也错，而正文看起来完全正常。
    test('扩展色的参数不会漏成 SGR 码', () {
      final spans = parseAnsi('\x1B[38;5;196m危险\x1B[0m 正常', base: _base);
      expect(spans.map((s) => s.text).join(), '危险 正常');
      expect(
        spans.last.style?.color,
        _base.color,
        reason: '`0` 之后必须回到基准色；扩展色参数漏成 SGR 的话这里会是别的颜色',
      );
    });

    /// `ESC[m` 是合法的重置写法（空参数等价于 `0`）。
    /// 漏掉它的后果是重置失效，此后所有文字都带着上一段的颜色。
    test('空参数等价于重置', () {
      final spans = parseAnsi('\x1B[31m红\x1B[m回来', base: _base);
      expect(spans.last.style?.color, _base.color);
    });

    /// `stripAnsi` 给「一行摘要」那种没地方放颜色的场合用。
    test('stripAnsi 只留字', () {
      expect(stripAnsi('\x1B[1;33m警告\x1B[0m：磁盘满了'), '警告：磁盘满了');
    });

    /// 一段真实的 cargo 输出（照抄格式，含粗体绿与重置）。
    test('真实终端输出：字全在，颜色分段', () {
      const real =
          '\x1B[1m\x1B[32m   Compiling\x1B[0m cortex-agent v0.1.9\n'
          '\x1B[1m\x1B[32m    Finished\x1B[0m `dev` profile';
      final spans = parseAnsi(real, base: _base);
      final joined = spans.map((s) => s.text).join();
      expect(joined, contains('Compiling'));
      expect(joined, contains('cortex-agent v0.1.9'));
      expect(joined, isNot(contains('\x1B')));
      expect(
        spans.length,
        greaterThan(1),
        reason: '带色的输出必须被切成多段，只有一段说明 SGR 根本没生效',
      );
    });
  });
}
