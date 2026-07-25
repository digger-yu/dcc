// P2.6: placement new 走裸 malloc 替身 (Section 6).
// 验证 dcc parser 检测 `new (ptr) T(args)` 形式, lower 阶段
// emit `((void*)(ptr), mangle_ctor(&n, ...)(ptr, args))` -
// 即不分配新内存, 在已有 ptr 上构造 T, 模拟 Itanium C++ ABI
// placement new 语义 (M17.0 阶段实装).
struct Token {
    int val;
};
int test_placement_new_basic() {
    // 已有 storage, 在 storage 上构造 Token
    int storage = 0;
    int* p = &storage;
    Token* t = new (p) Token;
    t->val = 100;
    return t->val; // 期望 100
}
int test_placement_new_with_args() {
    int storage = 0;
    int* p = &storage;
    // 注意: 真实 ABI 要求 ctor 与 storage 类型匹配;
    // dcc 简化实现接受 int 强转, 仅验证 lower 路径
    Token* t = new (p) Token(42);
    return t->val; // 期望 42
}
int test_placement_new_entry() {
    return test_placement_new_basic() + test_placement_new_with_args();
}
