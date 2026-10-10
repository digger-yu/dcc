// 测试嵌套函数静态链

int test_nested_static_chain() {
    int outer_var = 42;
    int outer_var2 = 100;

    // 嵌套函数访问外层变量
    int nested_func() {
        return outer_var + outer_var2;
    }

    return nested_func();
}

int main() {
    int result = test_nested_static_chain();
    if (result != 142) return 1;
    return 0;
}
