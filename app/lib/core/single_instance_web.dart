/// Web 上没有单实例这回事：多开标签页是**合法**用法，每个标签各有一份
/// 内存态，共享的持久层（服务端、localStorage）本来就按并发设计。
/// 这里恒放行，让 `main()` 两个平台走同一行代码。
bool ensureSingleInstance({String? dirOverride}) => true;
