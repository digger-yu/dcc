// 测试: 静态局部数组与文件级全局数组同名时的正确寻址
// 验证 resolve_global_key 在数组下标访问中正确解析 static local -> mangled 名
//
// 注意: 此测试不使用 for 循环内的同名引用, 因为解析器的 shadow map
// 不传播到嵌套块 (apply_shadow_map_stmt 在 Stmt::Block 处停止),
// 导致循环体内的 x 引用未被重命名为 shadow 名。这是独立的解析器 bug。

// 文件级全局数组
int x[8] = {100, 200, 300, 400, 500, 600, 700, 800};

// 测试 static local 数组: 常量索引
int test_const_index(void) {
    static int x[4] = {10, 20, 30, 40};
    return x[0] + x[1] + x[2] + x[3]; // 应为 100
}

// 测试 static local 数组: 运行时索引 (通过函数参数)
int test_runtime_index(int i) {
    static int x[4] = {10, 20, 30, 40};
    return x[i];
}

// 测试 static local 指针
int test_static_pointer(void) {
    static int *p;
    int vals[3] = {1, 2, 3};
    p = vals;
    return p[0] + p[1] + p[2]; // 应为 6
}

// 测试 static local 数组赋值
int test_store(int i, int val) {
    static int x[4] = {0};
    x[i] = val;
    return x[i];
}

// 测试取 static local 数组元素地址
int test_addr(void) {
    static int x[4] = {5, 6, 7, 8};
    int *p = &x[2];
    return *p; // 应为 7
}

// 确保全局数组仍然正确
int test_global(void) {
    return x[0] + x[7]; // 应为 100 + 800 = 900
}

int main(void) {
    if (test_const_index() != 100) return 1;
    if (test_runtime_index(0) != 10) return 2;
    if (test_runtime_index(3) != 40) return 3;
    if (test_static_pointer() != 6) return 4;
    if (test_store(1, 99) != 99) return 5;
    if (test_addr() != 7) return 6;
    if (test_global() != 900) return 7;
    return 0;
}
