/// Web 上没有单实例这回事：多开标签页是**合法**用法，每个标签各有一份
/// 内存态，共享的持久层（服务端、localStorage）本来就按并发设计。
/// 这里恒放行，让 `main()` 两个平台走同一行代码。
bool ensureSingleInstance({String? dirOverride}) => true;

/// Web 上永远走不到这里（上面恒 true）。留一个空实现只为让 `main()`
/// 两个平台共用同一段代码 —— 在 `main()` 里写 `if (!kIsWeb)` 的话，
/// 那个判断会和 `single_instance.dart` 的条件导入表达同一件事两遍。
void exitDuplicateInstance() {}
