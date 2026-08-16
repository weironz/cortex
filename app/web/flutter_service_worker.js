// ── 自杀版 service worker ────────────────────────────────────
//
// 这个应用不再使用 service worker（构建带 --pwa-strategy=none）。
// 这个文件存在的唯一理由，是**替换掉历史版本装进用户浏览器里的那一个**。
//
// 为什么普通的「新版本发布」到不了用户：旧 SW 连 index.html 都从自己的
// Cache Storage 里给（2026-08-16 实测：用户页面标题是新代码、错误文案是
// 三版之前的旧代码，硬刷也无济于事）。但浏览器对 **SW 脚本本身**的更新
// 检查是绕过 SW 直接打服务器的 —— 这是规范行为，旧 SW 拦不住。
// 于是它字节一变，浏览器就安装这个版本；它一激活就：
//   清光全部缓存 → 卸载自己 → 强制所有窗口重新导航。
// 那之后用户拿到的每个字节都来自服务器，且再也没有 SW。
//
// 这个文件要一直留着：谁也不知道哪个浏览器里还睡着一个旧 SW。

self.addEventListener('install', function () {
  // 立刻接管，不排队等旧 SW 的窗口全部关闭 —— 等下去就是「永远差一次重启」
  self.skipWaiting();
});

self.addEventListener('activate', function (event) {
  event.waitUntil(
    caches.keys()
      .then(function (keys) {
        return Promise.all(keys.map(function (k) { return caches.delete(k); }));
      })
      .then(function () { return self.registration.unregister(); })
      .then(function () { return self.clients.matchAll({ type: 'window' }); })
      .then(function (clients) {
        clients.forEach(function (c) { c.navigate(c.url); });
      })
  );
});

// 存在期间不拦任何请求
self.addEventListener('fetch', function () {});
