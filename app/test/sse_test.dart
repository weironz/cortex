import 'dart:convert';

import 'package:cortex_app/api/sse.dart';
import 'package:flutter_test/flutter_test.dart';

/// The SSE parser is the one piece of protocol code we own outright, and its
/// failure modes (chunk boundaries, keep-alive comments) are exactly the ones
/// that only show up against a real network.
void main() {
  Stream<List<int>> chunks(List<String> parts) =>
      Stream.fromIterable(parts.map(utf8.encode));

  test('decodes well-formed frames', () async {
    final events = await decodeSse(
      chunks([
        'data: {"type":"delta","text":"a"}\n\n',
        'data: {"type":"done"}\n\n',
      ]),
    ).toList();

    expect(events, hasLength(2));
    expect(events.first.data, '{"type":"delta","text":"a"}');
  });

  test('reassembles a frame split across chunk boundaries', () async {
    // The network, not the sender, decides where chunks break — a naive
    // per-chunk parser drops the second half of this event.
    final events = await decodeSse(
      chunks(['data: {"type":"del', 'ta","text":"hi"}\n', '\n']),
    ).toList();

    expect(events, hasLength(1));
    expect(events.single.data, '{"type":"delta","text":"hi"}');
  });

  test('ignores keep-alive comment lines', () async {
    // cortexd sends `:ping` every 15s; these must not surface as events.
    final events = await decodeSse(
      chunks([':ping\n\n', 'data: {"x":1}\n\n', ': another\n\n']),
    ).toList();

    expect(events, hasLength(1));
    expect(events.single.data, '{"x":1}');
  });

  test('joins multiple data lines within one frame', () async {
    final events = await decodeSse(
      chunks(['data: line1\ndata: line2\n\n']),
    ).toList();

    expect(events.single.data, 'line1\nline2');
  });

  test('handles CRLF line endings and event names', () async {
    final events = await decodeSse(
      chunks(['event: memory\r\ndata: {"a":1}\r\n\r\n']),
    ).toList();

    expect(events.single.event, 'memory');
    expect(events.single.data, '{"a":1}');
  });

  test('dispatches a trailing frame that has no blank line', () async {
    final events = await decodeSse(chunks(['data: tail'])).toList();
    expect(events.single.data, 'tail');
  });

  group('keepAlive: true —— 判活的那条路', () {
    test('心跳浮上来，且带着「我不是数据」的标记', () async {
      final events = await decodeSse(
        chunks([':ping\n\n', 'data: {"x":1}\n\n', ': another\n\n']),
        keepAlive: true,
      ).toList();

      expect(
        events.map((e) => e.isKeepAlive).toList(),
        [true, false, true],
        reason:
            '心跳与数据必须分得开 —— 混起来的话，判活会把一条 `: ping` '
            '当成模型回了一个字',
      );
      expect(
        events.where((e) => !e.isKeepAlive).single.data,
        '{"x":1}',
        reason: '打开判活不该改变数据帧本身的解析',
      );
      expect(
        events.where((e) => e.isKeepAlive).every((e) => e.data.isEmpty),
        isTrue,
        reason: '心跳没有 data —— 有的话调用方那句 `data.isEmpty` 就漏它过去了',
      );
    });

    test('默认仍然一个心跳都不发 —— 别的调用方（导入）不受影响', () async {
      final events = await decodeSse(chunks([':ping\n\n'])).toList();
      expect(events, isEmpty, reason: '不显式要的话，注释按规范就该被丢掉');
    });

    test('夹在一帧中间的心跳不会吃掉那一帧', () async {
      // 规范允许注释出现在帧内部。心跳会排到那一帧**前面**（它是立刻发的），
      // 判活不在乎顺序 —— 但数据一个字节都不能少
      final events = await decodeSse(
        chunks(['data: a\n:ping\ndata: b\n\n']),
        keepAlive: true,
      ).toList();

      expect(events.map((e) => e.isKeepAlive).toList(), [true, false]);
      expect(events.last.data, 'a\nb');
    });
  });

  test('carries multi-byte UTF-8 split across chunks', () async {
    // '记' is 3 bytes; splitting it must not produce a replacement char.
    final bytes = utf8.encode('data: {"t":"记忆"}\n\n');
    final events = await decodeSse(
      Stream.fromIterable([bytes.sublist(0, 12), bytes.sublist(12)]),
    ).toList();

    expect(events.single.data, '{"t":"记忆"}');
  });
}
