/// 用户可见文案里**不许再承诺长期记忆**。
///
/// # 为什么要一条测试盯着
///
/// 长期记忆 2026-08-17 整条拆去了 Cormex（见 CLAUDE.md）。而界面与随包
/// README 里留下了一串「这段时间不会有记忆」「本机对话与记忆不受影响」
/// 「任何人都拥有全部记忆」—— 每一句都在暗示**联网时有记忆**。
///
/// 这与 CLAUDE.md 那条「提示词与工具目录只能写当下真的成立的能力」是同一个
/// 错，只是发生在文案上：**留着「等它修好」的代价不是零，是每一次安装、
/// 每一次打开登录屏都在骗人。**
///
/// 这条测试盯的不是措辞，是那个承诺不要再回来 —— 措辞可以改，
/// 「界面上说有记忆」不行。
library;

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';

/// 只扫**用户看得见的字符串**，注释里可以自由讨论记忆（讨论历史是好事）。
///
/// 判据：这一行是字符串字面量的一部分，且不是以 `//` 开头的注释行。
Iterable<(String file, int line, String text)> _userFacingLines(
  Directory dir,
) sync* {
  for (final f in dir.listSync(recursive: true).whereType<File>()) {
    if (!f.path.endsWith('.dart')) continue;
    // mock 夹具里的假会话内容不算产品文案 —— 那是演示数据
    if (f.path.contains('mock_cortex_api')) continue;
    final lines = f.readAsLinesSync();
    for (var i = 0; i < lines.length; i++) {
      final raw = lines[i];
      if (raw.trimLeft().startsWith('//')) continue;
      if (!raw.contains("'") && !raw.contains('"')) continue;
      yield (f.path, i + 1, raw);
    }
  }
}

/// 产品标语「记忆原生的…AI Agent」—— **待定，不是漏网的**。
///
/// 它出现在三处（空会话副标题、空状态大标题、关于框）。与上面那些不同：
/// 那些是**行为承诺**（「会抽取记忆」「离线时没有记忆」），确凿是假的；
/// 而这一句是**产品定位**。
///
/// 三个事实摆在这里，决定权不在这条测试：
///  1. CLAUDE.md 第一行现在写的是「通用 AI Agent（编码 + 办公）：桌面与云
///     同一份 agent 循环，带 OS 级沙箱与权限，会话跨设备实时同步」——
///     已经没有「记忆原生」四个字了
///  2. 但 CLAUDE.md 同时写着记忆「要接回来，先解决那条认证」——
///     也就是说它可能是**有意保留的方向**，不是忘了改
///  3. 而一个装机用户读到「记忆原生」，会期待它跨会话记得住事情。它不会
///
/// 所以列在这里而不是从判据里悄悄排除掉：**排除掉就没人记得这件事了。**
/// 定了之后要么改标语、要么把这个列表连同这段注释一起删掉。
const _pendingProductTagline = '记忆原生';

void main() {
  test('界面文案里不再承诺长期记忆', () {
    final offenders = <String>[];
    for (final (file, line, text) in _userFacingLines(Directory('lib'))) {
      if (text.contains('记忆')) {
        // 「不是记忆服务本身」是**路由说明**，不是能力承诺：它在教用户
        // 别把 Cormex 的地址填进部署入口。留着是对的
        if (text.contains('不是记忆服务本身')) continue;
        // 产品标语待定，见上面那段
        if (text.contains(_pendingProductTagline)) continue;
        offenders.add('$file:$line  ${text.trim()}');
      }
    }
    expect(
      offenders,
      isEmpty,
      reason:
          '这些用户可见文案还在提长期记忆，而它 2026-08-17 已经整条拆去 Cormex。\n'
          '说「离线时没有记忆」等于暗示联网时有 —— 承诺一个不存在的功能。\n'
          '${offenders.join('\n')}',
    );
  });

  test('随包 README 不再承诺长期记忆，且写清数据留在哪', () {
    final script = File('../scripts/release-desktop-windows.sh');
    expect(script.existsSync(), isTrue, reason: '打包脚本应该在');
    final body = script.readAsStringSync();
    // 只看生成 README.txt 的那一段
    final start = body.indexOf('Cortex 桌面端 v');
    final end = body.indexOf('许可证：Apache-2.0');
    expect(start, greaterThan(0));
    expect(end, greaterThan(start));
    final readme = body.substring(start, end);

    expect(
      readme.contains('记忆'),
      isFalse,
      reason:
          '随包 README 是装机用户读到的第一份文档。'
          '它此前写着「记忆…是唯一的权威副本」「界面上会明说记忆未连接」——'
          '而那个字符串在界面里一个都搜不到了',
    );
    expect(
      readme.contains(r'%LOCALAPPDATA%\cortex'),
      isTrue,
      reason:
          '卸载不删数据目录，那就必须告诉用户它在哪 —— '
          '否则想彻底清干净的人无从下手（Ollama / Claude Code 都是这么做的）',
    );
    expect(
      readme.contains('CLI'),
      isTrue,
      reason:
          'CLI 与桌面端共用那个目录：只删桌面端的话，CLI 一跑又会重建。'
          '不说这句，用户会以为自己没删干净',
    );
  });
}
