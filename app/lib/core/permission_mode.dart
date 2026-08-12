/// 这一轮要打扰用户到什么程度。
///
/// 与 `cortex_proto::dto::PermissionMode` 一一对应，[wire] 就是线上那个字符串。
///
/// # 为什么是三档，而不是照抄 Claude Code 的五档
///
/// 它的菜单里还有 Plan（先出方案再动手）与 Auto（模型自己判断）。那两个各自
/// 是独立功能 —— 前者要一整套「计划-批准-执行」的流程，后者要一个判定器 ——
/// 与「问不问」无关。塞进同一个枚举只会让这里看起来做完了，
/// 而实际上有两个空壳。
enum PermissionMode {
  /// 逐条确认：写文件与执行命令都问，越界也问。**默认**。
  ask('ask', '逐条确认', '写文件、执行命令、碰工作区外的路径，都先问你一句'),

  /// 自动改文件：写不问，执行才问。
  ///
  /// **越界仍然问** —— 越界与风险是两件独立的事，这一档只关掉后者。
  acceptEdits('accept_edits', '自动改文件', '改工作区里的文件不再打断你；执行命令和碰工作区外的路径仍然会问'),

  /// 完全放行：一律不问，越界也不问。对齐 "Bypass permissions"。
  bypass('bypass', '完全放行', '什么都不问，包括执行命令和工作区外的路径');

  const PermissionMode(this.wire, this.label, this.blurb);

  /// 线上取值。服务端是 `#[serde(rename_all = "snake_case")]`。
  final String wire;

  /// chip 上与菜单里显示的名字。
  final String label;

  /// 菜单里那行解释。**必须说清「哪些还会问」** —— 一个只写「更省事」的
  /// 描述会让人以为自动改文件档也不问 shell 了。
  final String blurb;

  /// 从存下来的字符串还原。认不出就回到最谨慎的一档。
  ///
  /// 认不出的来源：老版本存下的值、手改坏的配置文件、将来删掉的档位。
  /// 回落方向刻意是**问**：反过来的话，一个读不懂的字符串会静默把 agent
  /// 变成无人值守的。
  static PermissionMode fromWire(String? s) => PermissionMode.values.firstWhere(
    (m) => m.wire == s,
    orElse: () => PermissionMode.ask,
  );
}
