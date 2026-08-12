/// 版本号比较。单独一个文件，因为它是这条链上**唯一**能被纯函数钉死的地方。
///
/// # 为什么不直接比字符串
///
/// `'0.1.10'.compareTo('0.1.9')` 是负数 —— 字典序里 `'1' < '9'`。于是发到
/// 0.1.10 的那天，所有 0.1.9 的用户都收不到更新，而且**没有任何报错**：
/// 更新器工作正常，只是永远认为自己已经是最新的。这个 bug 会一直睡到
/// 版本号进位那一刻，然后以「更新功能坏了」的形式醒来。
library;

/// 把 `v0.1.7` / `0.1.7+8` / `0.1.7-rc.1` 解析成 `[0, 1, 7]`。
///
/// 只取前三段数字：`+8` 是 Flutter 的 build number（同一个版本重打一次包也会
/// 变），`-rc.1` 是预发布标记。两者都不该让「是不是新版本」这个判断改口 ——
/// 我们比的是发布版本，不是构建标识。
///
/// 解析不出来时返回 null，**不抛异常**：这个值来自网络（GitHub 的 tag_name），
/// 一个畸形的 tag 不该让应用崩掉，它只该让「有没有更新」这件事回答不知道。
List<int>? parseVersion(String raw) {
  var s = raw.trim();
  if (s.isEmpty) return null;
  if (s.startsWith('v') || s.startsWith('V')) s = s.substring(1);

  // 先砍掉 build number 与预发布标记，剩下 `x.y.z`
  final cut = s.indexOf(RegExp(r'[+\-]'));
  if (cut >= 0) s = s.substring(0, cut);

  final parts = s.split('.');
  if (parts.isEmpty || parts.length > 3) return null;

  final out = <int>[];
  for (final p in parts) {
    final n = int.tryParse(p);
    // 负数会让 `-` 先被上面砍掉，所以这里只可能是非数字
    if (n == null || n < 0) return null;
    out.add(n);
  }
  // 补齐到三段：`0.2` 与 `0.2.0` 是同一个版本
  while (out.length < 3) {
    out.add(0);
  }
  return out;
}

/// [latest] 是否比 [current] 新。
///
/// 任何一边解析不出来都返回 **false** —— 「不知道」必须落到「不提示更新」，
/// 而不是「提示更新」。前者的代价是用户晚几天升级，后者的代价是拿一个
/// 来路不明的版本号去覆盖用户手上能跑的那份。
bool isNewer({required String current, required String latest}) {
  final a = parseVersion(current);
  final b = parseVersion(latest);
  if (a == null || b == null) return false;
  for (var i = 0; i < 3; i++) {
    if (b[i] != a[i]) return b[i] > a[i];
  }
  return false; // 完全相同
}
