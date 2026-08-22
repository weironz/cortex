/// 这一轮如果画图，按什么规格画。
///
/// # ⚠️ 它是**兜底**，不是覆盖
///
/// 模型自己在工具参数里填了尺寸就听模型的 —— 用户那句「画一张宽的」是
/// **这一次**的意图，比规格面板上那个留着没动的值更近。覆盖的表现是他说的
/// 话被一个他早就忘了的设置静默否决，而界面上没有任何地方解释。
///
/// 张数没有这个问题：工具参数里根本没有它，模型表达不了。
///
/// 判据与服务端 `cortex_local::turn::resolve_image_spec` 同源。
library;

class ImagePrefs {
  const ImagePrefs({this.size, this.count = 1});

  /// `宽*高`。`null` = 让模型自己按提示词推荐。
  final String? size;
  final int count;

  /// 什么都没设时**整个字段不发** —— 发一个 `{size: null, n: 1}` 与不发
  /// 在服务端是同一个意思，少一个字段少一处要解释的东西。
  bool get isDefault => size == null && count <= 1;

  Map<String, Object?> toJson() => {'size': ?size, 'n': count};
}
