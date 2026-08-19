import 'package:flutter_riverpod/flutter_riverpod.dart';

/// 要塞进输入框的一段草稿。
///
/// # 为什么要一个 provider，而不是直接给输入框传参
///
/// 「改一改再发一次」这个动作的发起点在**消息气泡**里，而输入框是它的
/// 兄弟节点 —— 两者之间没有父子关系可以传参。把 `TextEditingController`
/// 提到共同祖先上也能做到，但那会让每一次敲键盘都重建整棵对话树。
///
/// # 为什么带一个序号
///
/// 光存文本的话，「同一句话连点两次改一改」第二次不会生效：state 没变，
/// 监听方不会被通知。序号让每一次请求都是一个新值 —— 这是本仓库
/// 「同样的值不触发」踩过的形状，在这里提前避开。
class ComposerDraft {
  const ComposerDraft({required this.text, required this.seq});

  final String text;
  final int seq;
}

class ComposerDraftController extends Notifier<ComposerDraft?> {
  int _seq = 0;

  @override
  ComposerDraft? build() => null;

  /// 请求把 [text] 放进输入框并把光标停在末尾。
  void offer(String text) {
    _seq++;
    state = ComposerDraft(text: text, seq: _seq);
  }

  /// 输入框已经吃下这段草稿。
  ///
  /// 吃完就清掉：留着的话，用户切走再切回来会被同一段草稿覆盖第二次 ——
  /// 而那时他可能已经在写别的东西了。
  void consume() => state = null;
}

final composerDraftProvider =
    NotifierProvider<ComposerDraftController, ComposerDraft?>(
      ComposerDraftController.new,
    );
