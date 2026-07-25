// P2.3: C++ 模板实参语法 T<U> (Section 6 / 模板实例化行)
// 验证 dcc cpp 解析器接受 `std::vector<int>` / `Map<K, V>` 等
// 模板实例化语法 (M18 阶段实装 parse_mangle_type 的 `<` 识别
// + looks_like_template_args_after_lt 嵌套判断). 完整单态化
// 仍走 M10+ `try_substitute` 简化版, 本 fixture 只验证
// 解析 + lower 阶段不报错.

struct Box {
    int v;
    Box(int x) : v(x) {}
};

// 模板类: 仅声明, 不要求单态化
template <class T> struct Holder {
    T payload;
};

// 用 T<U> 语法声明指针/引用, 仅走解析路径
int test_template_arg_parse() {
    Holder<int>* h = 0;
    // 嵌套模板实参: `Holder<Holder<int>>*` 需要 lexer 把
    // 末尾的 `>>` 正确处理 (我们 M18 把 `Shr` 视作两层 `>`).
    Holder<Holder<int>>* hh = 0;
    Box b(7);
    int x = b.v;
    // 用 `(void)` 抑制 unused warnings
    (void)h;
    (void)hh;
    return x; // 期望 7
}
