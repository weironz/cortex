/// `SKILL.md` 的解析与写出。
///
/// # 这一组在盯的三件事
///
/// 1. **别人写的文件我们读得进来。** 兼容这份格式的全部意义就是
///    「同一份 skill 三家都能装」—— 读不进来的话，用户得手抄一遍。
/// 2. **正文里的 `---` 不能被当成 frontmatter。** markdown 里一条分隔线
///    很常见，认错的症状是正文被从中间劈开。
/// 3. **写出去的文件别人读得懂。** 名字里带 `: ` 时不加引号，
///    别的解析器会把它读成一个嵌套映射。
library;

import 'package:cortex_app/core/skill_md.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('标准 frontmatter 读得出三段', () {
    final s = parseSkillMd('''
---
name: 处理 CSV
description: 把表格清洗成规整的行
---

先看表头，再按列判断类型。
''')!;
    expect(s.name, '处理 CSV');
    expect(s.description, '把表格清洗成规整的行');
    expect(s.instructions, '先看表头，再按列判断类型。');
  });

  test('带引号的值要脱引号', () {
    final s = parseSkillMd('''
---
name: "处理 CSV: 分列"
description: '清洗'
---
正文
''')!;
    expect(
      s.name,
      '处理 CSV: 分列',
      reason: '别人写出来的文件里，带冒号的值必然是引起来的 —— 不脱引号的话名字里会多两个双引号',
    );
    expect(s.description, '清洗');
  });

  test('没有 frontmatter 时拿一级标题当名字', () {
    final s = parseSkillMd('# 部署检查单\n\n先跑 just ci。')!;
    expect(
      s.name,
      '部署检查单',
      reason: '一份手写的 markdown 做法也该能直接用起来 —— 强制要 frontmatter 等于把它挡在门外',
    );
    expect(s.instructions, contains('just ci'));
  });

  test('正文里的分隔线不算 frontmatter', () {
    final s = parseSkillMd('''
# 做法

第一步

---

第二步
''')!;
    expect(
      s.instructions,
      contains('第一步'),
      reason: '只认文件开头那一段。不限位置的话，正文里一条分隔线会把内容从中间劈开',
    );
    expect(s.instructions, contains('第二步'));
  });

  test('连名字都找不到时返回 null，不编一个', () {
    expect(
      parseSkillMd('就是一段没有标题的文字'),
      isNull,
      reason: '无名的技能取不回来（load_skill 按名字拿）—— 编一个名字等于造一条用不了的记录',
    );
    expect(parseSkillMd('一段文字', fallbackName: 'deploy.md')?.name, 'deploy.md');
  });

  test('写出去再读回来是同一份', () {
    const name = '处理 CSV: 分列';
    const desc = '清洗表格';
    const body = '第一步\n\n第二步';
    final round = parseSkillMd(
      toSkillMd(name: name, description: desc, instructions: body),
    )!;
    expect(round.name, name, reason: '往返不等于原样的话，导出再导入会静默改掉用户的技能名');
    expect(round.description, desc);
    expect(round.instructions, body);
  });
}
