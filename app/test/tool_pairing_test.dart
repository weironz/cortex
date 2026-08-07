import 'package:cortex_app/models/tool_call.dart';
import 'package:flutter_test/flutter_test.dart';

/// The wire emits two `tool` events per invocation. These pin down the folding
/// rule, because getting it wrong is invisible in a screenshot: you just see
/// one extra grey line per tool and assume the agent called it twice.
void main() {
  test('a call and its result collapse into one row', () {
    var calls = <ToolCall>[];
    calls = ToolCall.merge(
      calls,
      'read_file',
      '调用 read_file (path=src/main.rs)',
    );

    expect(calls, hasLength(1));
    expect(
      calls.single.pending,
      isTrue,
      reason: '结果事件还没到，这一行必须显示为执行中',
    );
    expect(
      calls.single.arguments,
      '(path=src/main.rs)',
      reason: '成对显示后行首已有工具名，摘要里的「调用 read_file」是冗余的',
    );

    calls = ToolCall.merge(calls, 'read_file', 'read_file 返回 12 行 / 340 字符');

    expect(calls, hasLength(1), reason: '第二条事件是同一次调用的结果，不能新起一行');
    expect(calls.single.pending, isFalse);
    expect(calls.single.result, '返回 12 行 / 340 字符');
    expect(calls.single.arguments, '(path=src/main.rs)');
    expect(calls.single.failed, isFalse);
  });

  test('the same tool called twice yields two rows', () {
    var calls = <ToolCall>[];
    calls = ToolCall.merge(calls, 'memory_search', '调用 memory_search (query=a)');
    calls = ToolCall.merge(calls, 'memory_search', 'memory_search 返回 3 行 / 90 字符');
    calls = ToolCall.merge(calls, 'memory_search', '调用 memory_search (query=b)');

    expect(
      calls,
      hasLength(2),
      reason: '第一行已经拿到结果，第三条事件只能是一次新的调用',
    );
    expect(calls.first.arguments, '(query=a)');
    expect(calls.first.pending, isFalse);
    expect(calls.last.arguments, '(query=b)');
    expect(calls.last.pending, isTrue);
  });

  test('interleaved tools do not steal each other results', () {
    var calls = <ToolCall>[];
    calls = ToolCall.merge(calls, 'read_file', '调用 read_file (path=a)');
    calls = ToolCall.merge(calls, 'read_file', 'read_file 返回 4 行 / 20 字符');
    calls = ToolCall.merge(calls, 'list_dir', '调用 list_dir (path=.)');
    calls = ToolCall.merge(calls, 'list_dir', 'list_dir 返回 9 行 / 88 字符');

    expect(calls.map((c) => c.name), ['read_file', 'list_dir']);
    expect(calls.every((c) => !c.pending), isTrue);
  });

  test('a failed result is marked, not swallowed', () {
    var calls = <ToolCall>[];
    calls = ToolCall.merge(calls, 'write_file', '调用 write_file (path=/etc/x)');
    calls = ToolCall.merge(
      calls,
      'write_file',
      'write_file 失败：路径 /etc/x 在工作区之外，已拒绝',
    );

    expect(calls.single.failed, isTrue);
    expect(calls.single.result, startsWith('失败：'));
  });

  test('a summary without the expected prefix survives intact', () {
    // Forward compatibility: the daemon may reword these strings, and dropping
    // the text because it no longer matches a prefix would be worse than
    // showing it verbatim.
    final calls = ToolCall.merge(const [], 'shell', 'ran something');
    expect(calls.single.arguments, 'ran something');
  });
}
