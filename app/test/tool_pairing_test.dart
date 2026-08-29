import 'package:cortex_app/models/chat_event.dart';
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
    expect(calls.single.pending, isTrue, reason: '结果事件还没到，这一行必须显示为执行中');
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
    calls = ToolCall.merge(
      calls,
      'memory_search',
      '调用 memory_search (query=a)',
    );
    calls = ToolCall.merge(
      calls,
      'memory_search',
      'memory_search 返回 3 行 / 90 字符',
    );
    calls = ToolCall.merge(
      calls,
      'memory_search',
      '调用 memory_search (query=b)',
    );

    expect(calls, hasLength(2), reason: '第一行已经拿到结果，第三条事件只能是一次新的调用');
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

  group('子 agent 并行时的配对', () {
    SubagentTag tag(int i) => SubagentTag(index: i, task: '任务 $i');

    /// **四路并行下，结果必须接回自己那条调用。**
    ///
    /// 这是这个功能里唯一一处「错了完全看不出来」的地方：merge 原本只看
    /// `calls.last`，而子 agent 是并行的 —— A 开工、B 开工、A 收工 是常态。
    /// 只看最后一条的话，A 的结果会显示在 B 的工具名下，而界面上那一行
    /// 看起来完全正常（有工具名、有结果、不报错），只是**接错了人**。
    test('交错到达时，A 的结果不会接到 B 的调用上', () {
      var calls = <ToolCall>[];
      calls = ToolCall.merge(
        calls,
        'read_file',
        '调用 read_file (a.rs)',
        subagent: tag(1),
      );
      calls = ToolCall.merge(calls, 'grep', '调用 grep (x)', subagent: tag(2));
      // 1 号先收工 —— 此时 calls.last 是 2 号那条
      calls = ToolCall.merge(
        calls,
        'read_file',
        'read_file 读了 30 行',
        phase: ToolPhase.result,
        subagent: tag(1),
      );

      expect(calls, hasLength(2), reason: '不该多出一行 —— 它该接到 1 号那条上');
      expect(
        calls.first.pending,
        isFalse,
        reason: '1 号那条该完成了；还 pending 说明结果没接上，界面上它会永远转圈',
      );
      expect(calls.first.subagent?.index, 1);
      expect(
        calls.last.pending,
        isTrue,
        reason:
            '2 号还没收工，它必须原样待着 —— 被 1 号的结果顶掉是最坏的那种：'
            '界面显示 2 号干完了，而它其实还在跑',
      );
    });

    /// 同名工具、不同 agent —— 最容易配错的组合。
    test('两个子 agent 调同一个工具时各归各的', () {
      var calls = <ToolCall>[];
      calls = ToolCall.merge(
        calls,
        'read_file',
        '调用 read_file',
        subagent: tag(1),
      );
      calls = ToolCall.merge(
        calls,
        'read_file',
        '调用 read_file',
        subagent: tag(2),
      );
      calls = ToolCall.merge(
        calls,
        'read_file',
        'read_file 完成',
        phase: ToolPhase.result,
        subagent: tag(2),
      );

      expect(calls, hasLength(2));
      expect(calls[0].pending, isTrue, reason: '1 号还没收工');
      expect(calls[1].pending, isFalse, reason: '收工的是 2 号');
    });

    /// 主 agent 那些行照旧 —— 这个改动不该动到它。
    test('主 agent 的行为一字不变', () {
      var calls = <ToolCall>[];
      calls = ToolCall.merge(calls, 'read_file', '调用 read_file');
      calls = ToolCall.merge(
        calls,
        'read_file',
        'read_file 完成',
        phase: ToolPhase.result,
      );
      expect(calls, hasLength(1));
      expect(calls.single.pending, isFalse);
      expect(calls.single.subagent, isNull);
    });

    /// 子 agent 的结果**不许**接到主 agent 那条同名调用上。
    test('主 agent 与子 agent 的同名调用互不串台', () {
      var calls = <ToolCall>[];
      calls = ToolCall.merge(calls, 'read_file', '调用 read_file'); // 主
      calls = ToolCall.merge(
        calls,
        'read_file',
        'read_file 完成',
        phase: ToolPhase.result,
        subagent: tag(1),
      );
      expect(
        calls,
        hasLength(2),
        reason:
            '子 agent 的结果找不到自己那条调用时，该另起一行，'
            '而不是把主 agent 那条顶掉',
      );
      expect(calls.first.pending, isTrue, reason: '主 agent 那条还没收到自己的结果');
    });
  });
}
