/// 浏览器那一侧 —— `Notification`。见 `attention.dart`。
library;

import 'dart:js_interop';

import 'package:web/web.dart' as web;

/// 权限只问一次，问过的结果记在这里。
///
/// 不记的话，每一轮跑完都会重新走一次 `requestPermission` —— 浏览器
/// 对已经决定过的请求会立刻回旧答案，所以不弹框，但那是一次没必要的
/// 异步往返插在「一轮刚结束」这个最忙的时刻。
bool? _granted;

/// 弹一条系统通知。回 `false` = 没弹出来（不支持、被拒、或页面在前台）。
///
/// [title] 与 [body] 都会显示给用户，所以调用方要给人话，不是 id。
Future<bool> callAttention(String title, String body) async {
  try {
    // ⚠️ **页面在前台就不弹。** 用户正看着这个标签页的时候弹系统通知，
    // 是在替一件他已经看见的事情制造噪音 —— 与桌面那侧「已经在前台
    // 就不闪」是同一条判据，只是问法不同（那边问窗口句柄，这边问
    // `visibilityState`）
    if (web.document.visibilityState == 'visible') return false;

    // 不支持的浏览器（老 Safari、某些内嵌 WebView）—— 静默返回 false，
    // 不抛：调用方据此什么都别声称
    if (!web.Notification.permission.isNotEmpty) return false;

    if (_granted == null) {
      final current = web.Notification.permission;
      if (current == 'granted') {
        _granted = true;
      } else if (current == 'denied') {
        // 用户拒过了。**不再问** —— 浏览器也不会再弹，反复问只是浪费
        _granted = false;
      } else {
        // 'default'：还没问过。**这一刻才问**，而不是开机就问 ——
        // 一个刚打开就弹权限框的网站，十次有九次被点「拒绝」，
        // 然后这个能力就永久没了
        final r = await web.Notification.requestPermission().toDart;
        _granted = r.toDart == 'granted';
      }
    }
    if (_granted != true) return false;

    web.Notification(title, web.NotificationOptions(body: body));
    return true;
  } on Object {
    // 提醒失败不值得让一轮对话炸掉 —— 它是锦上添花
    return false;
  }
}
