/// 界面文案里不许出现字面 Markdown。
///
/// # 这条是从一次目视冒烟里来的
///
/// 0.1.6 发版前把桌面端真的跑了一次，看到登录卡片上写着
/// `**这段时间不会有记忆**` —— 星号原样显示给用户看。原因很简单：
/// `Text` 是纯文本组件，不渲染 Markdown。
///
/// **而当时所有 widget 测试都是绿的**：它们断言的是「界面上有这段文字」，
/// 而字面星号恰恰**在**那段文字里。测试与 bug 完全相容。
///
/// 所以这条不测行为，测**源码**：渲染 Markdown 要用 `CortexMarkdown`，
/// 强调要用 `Text.rich` + `FontWeight`，纯 `Text` 里不该有 `**`。
library;

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';

void main() {
  test('Text 组件的文案里没有字面 Markdown 星号', () {
    final offenders = <String>[];
    // 只扫界面层：models / api 里的字符串不进 Text
    for (final dir in ['lib/features', 'lib/widgets']) {
      final root = Directory(dir);
      if (!root.existsSync()) continue;
      for (final f in root.listSync(recursive: true).whereType<File>()) {
        if (!f.path.endsWith('.dart')) continue;
        var lineNo = 0;
        for (final line in f.readAsLinesSync()) {
          lineNo++;
          final trimmed = line.trimLeft();
          // 注释里随便写 —— 这个仓库的注释本来就用 Markdown 强调
          if (trimmed.startsWith('//')) continue;
          if (!line.contains('**')) continue;
          // 引号里才算文案
          if (!line.contains("'") && !line.contains('"')) continue;
          offenders.add('${f.path}:$lineNo: ${trimmed.trim()}');
        }
      }
    }
    expect(
      offenders,
      isEmpty,
      reason: '这些地方的 `**` 会原样显示给用户看：\n${offenders.join('\n')}\n'
          'Text 不渲染 Markdown。要强调用 Text.rich + FontWeight.w700；'
          '要整段 Markdown 用 CortexMarkdown。\n'
          '注意：widget 测试抓不到这个 —— 它们断言「界面上有这段文字」，'
          '而字面星号就在那段文字里',
    );
  });
}
