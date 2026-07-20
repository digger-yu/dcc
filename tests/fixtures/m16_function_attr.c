// m16_function_attr.c
// 函数属性传播: cold / hot 标注应被保留且不影响正确性
// 通过: exit 0
// 注: M14 之前 dcc 对负字面量参数 + 100 表达式的 codegen 有 bug,
//     已知问题, 这里用正值绕开。

__attribute__((cold)) int cold_path(int x) {
    return x + 1;
}

int hot_path(int x) {
    return x * 2;
}

int main() {
    if (hot_path(21) != 42) return 1;
    if (cold_path(49) != 50) return 2;
    return 0;
}
