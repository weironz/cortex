/// 粘贴时剪贴板里两样都有，取哪一样。
///
/// 只钉判据，钉不住接线 —— 真正的调用点在
/// `_MessageComposerState._paste` 的异步 IO 里（`Pasteboard.text` /
/// `Pasteboard.image` 都走 platform channel），从测试里够不着。
/// 这个局限与 `shouldForwardUnauthorized` 那一处是同一种，写在这里免得
/// 下一个人以为「粘贴这条路全被测过了」。
library;

import 'package:cortex_app/features/chat/widgets/message_composer.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('粘贴取文字还是取图', () {
    test('⚠️ 两样都有时取文字', () {
      expect(
        choosePaste(text: '一段话', hasImage: true),
        PasteChoice.text,
        // 从网页复制一段带插图的内容，剪贴板里就是两样都有。反过来的话
        // 用户得到一张莫名其妙的图、正文没了，还得手动删掉重新复制
        reason: '十次里九次用户要的是文字',
      );
    });

    test('只有图时取图', () {
      expect(choosePaste(text: null, hasImage: true), PasteChoice.image);
    });

    test('⚠️ 空字符串不算「有文字」', () {
      expect(
        choosePaste(text: '', hasImage: true),
        PasteChoice.image,
        // 这个仓库在「空串顶掉默认值」上栽过六次。这里的形状一样：
        // 有些平台在剪贴板里只有图时，text 那一路返回的是空串而不是 null，
        // 照单全收的话粘图这条路**永远走不到**
        reason: '空串当成「有文字」的话，粘图会静默失效且没有任何报错',
      );
    });

    test('两样都没有就什么都不做', () {
      expect(choosePaste(text: null, hasImage: false), PasteChoice.nothing);
      expect(choosePaste(text: '', hasImage: false), PasteChoice.nothing);
    });
  });
}
