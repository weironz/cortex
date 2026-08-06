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

  test('carries multi-byte UTF-8 split across chunks', () async {
    // '记' is 3 bytes; splitting it must not produce a replacement char.
    final bytes = utf8.encode('data: {"t":"记忆"}\n\n');
    final events = await decodeSse(
      Stream.fromIterable([
        bytes.sublist(0, 12),
        bytes.sublist(12),
      ]),
    ).toList();

    expect(events.single.data, '{"t":"记忆"}');
  });
}
