// tests/fixtures/c/c23_constexpr_var_error.c
// C23 constexpr 变量: 非常量初始化器应报错.

int get_value(void);

int main(void) {
    // constexpr 变量必须用常量表达式初始化
    // 使用函数调用应报错
    constexpr int bad = get_value();  // E0011: requires constant initializer
    return bad;
}
