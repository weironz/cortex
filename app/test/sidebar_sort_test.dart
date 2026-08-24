/// 左栏排序三档。
///
/// # 这一组在盯的两件事
///
/// 1. **「优先级」那一档排的是「下一步该看哪个」。** 顺序错了的话它与
///    「最近更新」看起来没区别 —— 一个没有区分度的档位，等于菜单里
///    多了一行噪音。
/// 2. **手动档里新建的会话不能消失。** 顺序表里没有它 —— 排到最后是对的，
///    掉出列表是事故（而这两种在实现上只差一个默认值）。
library;

import 'package:cortex_app/models/chat_session.dart';
import 'package:cortex_app/state/sidebar_sort.dart';
import 'package:flutter_test/flutter_test.dart';

ChatSession _s(String id, {bool pinned = false}) =>
    ChatSession(id: id, title: id, pinned: pinned);

void main() {
  group('优先级那一档', () {
    test('等你确认排最前，其次是跑完没看、在跑、置顶', () {
      final ranks = [
        priorityRank(
          awaitingConfirm: true,
          justFinished: false,
          streaming: true,
          pinned: false,
        ),
        priorityRank(
          awaitingConfirm: false,
          justFinished: true,
          streaming: false,
          pinned: false,
        ),
        priorityRank(
          awaitingConfirm: false,
          justFinished: false,
          streaming: true,
          pinned: false,
        ),
        priorityRank(
          awaitingConfirm: false,
          justFinished: false,
          streaming: false,
          pinned: true,
        ),
        priorityRank(
          awaitingConfirm: false,
          justFinished: false,
          streaming: false,
          pinned: false,
        ),
      ];
      expect(
        ranks,
        [0, 1, 2, 3, 4],
        reason:
            '「等你确认」是唯一不管就永远卡住的状态，必须最前；'
            '它同时也在跑，所以这一条还钉住了「确认压过 streaming」',
      );
    });

    test('同一档内保持最近更新在前', () {
      // ULID 字典序 = 生成时序，所以 id 大的更新
      final sorted = sortSessions(
        [_s('AAA'), _s('CCC'), _s('BBB')],
        const SidebarSortState(mode: SidebarSort.priority),
        awaiting: const {},
        finished: const {},
        running: const {},
      );
      expect(sorted.map((s) => s.id).toList(), [
        'CCC',
        'BBB',
        'AAA',
      ], reason: '都在同一档时，这一档不该把时间顺序打乱 —— 那会让列表看起来是随机的');
    });

    test('置顶排在普通会话之前，但让位于要你动手的那些', () {
      final sorted = sortSessions(
        [_s('AAA', pinned: true), _s('BBB')],
        const SidebarSortState(mode: SidebarSort.priority),
        awaiting: const {'BBB'},
        finished: const {},
        running: const {},
      );
      expect(
        sorted.first.id,
        'BBB',
        reason:
            '置顶是「我常用」，等你确认是「不处理就卡住」—— 后者更急。'
            '反过来的话，一个被钉住的闲置会话会把真正卡着的那条压到下面',
      );
    });
  });

  group('手动那一档', () {
    test('按顺序表排，不在表里的排后面而不是消失', () {
      final sorted = sortSessions(
        [_s('AAA'), _s('BBB'), _s('CCC')],
        const SidebarSortState(mode: SidebarSort.manual, order: ['CCC', 'AAA']),
        awaiting: const {},
        finished: const {},
        running: const {},
      );
      expect(
        sorted.map((s) => s.id).toList(),
        ['CCC', 'AAA', 'BBB'],
        reason:
            '新建的会话还没进过顺序表。排到最后是对的，掉出列表是事故 —— '
            '而这两种在实现上只差一个默认值',
      );
    });
  });

  test('最近更新那一档不重排 —— 服务端已经是这个序', () {
    final input = [_s('AAA'), _s('CCC'), _s('BBB')];
    final sorted = sortSessions(
      input,
      const SidebarSortState(),
      awaiting: const {},
      finished: const {},
      running: const {},
    );
    expect(
      sorted.map((s) => s.id).toList(),
      ['AAA', 'CCC', 'BBB'],
      reason:
          '客户端再排一遍的话，两处排法有细微差别时列表会在「刚拉回来」'
          '与「刷新后」之间跳一下',
    );
  });
}
