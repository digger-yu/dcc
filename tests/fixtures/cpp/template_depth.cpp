// M18.x: 模板嵌套深度 - 正常深度编译 smoke test
// 覆盖 parse_mangle_type 递归解析 T<T<...>> (含 `>>` 拆 token 闭合)
// 与模板深度限制 (template_depth.rs, MAX=128) 的合法范围.
// 8 层嵌套远低于限制, 应在类型上下文 (形参) 与表达式上下文
// (局部声明) 都能编译.

template <class T> struct Wrap { T v; };

// 类型上下文: 形参中的深层嵌套模板 (偶数层 `>>` 闭合)
int take_deep(Wrap<Wrap<Wrap<Wrap<Wrap<Wrap<Wrap<Wrap<int>>>>>>>>* p) {
    return p != 0;
}

int test_template_depth_ok() {
    // 表达式上下文: 局部声明中的深层嵌套模板
    Wrap<Wrap<Wrap<Wrap<Wrap<Wrap<Wrap<Wrap<int>>>>>>>>* deep = 0;
    Wrap<int>* single = 0;
    (void)deep;
    (void)single;
    if (take_deep(0) != 0) return 1;
    return 0;
}

int main() { return test_template_depth_ok(); }
