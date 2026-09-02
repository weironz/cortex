/// 收进资料库时那个名字怎么定。
///
/// 单独一支是因为两处都要用同一份规则：图片右键那一项、以及对话里
/// 文件附件那一项。两边各写一遍的下场是同一份东西从两个入口收进去，
/// 库里出现两个名字不同的条目 —— 而 `POST /library` 按哈希去重，
/// 于是**后收的那次静默回了前一条**，名字看起来「改不动」。
library;

/// 资料库里这一条叫什么。
///
/// 优先用 [label]（画它的那句提示词，或者附件的显示名）：在一屏缩略图里
/// 「一只戴眼镜的柯基」比 `cortex-3f8a1c2d.png` 有用得多。
///
/// 没有就退回哈希前八位 —— 与「另存为」同一个规则，两处对得上。
String libraryNameFor({required String hash, String? label}) {
  final s = label?.trim().replaceAll(RegExp(r'\s+'), ' ') ?? '';
  if (s.isEmpty) return 'cortex-${hash.substring(0, 8)}';
  // 按**码点**截而不是按 UTF-16 单元：`substring(0, 60)` 会把一个
  // 四字节字符（emoji、生僻字）劈成半个，落库的是一个替换字符
  final runes = s.runes.toList();
  if (runes.length <= 60) return s;
  return '${String.fromCharCodes(runes.take(60))}…';
}
