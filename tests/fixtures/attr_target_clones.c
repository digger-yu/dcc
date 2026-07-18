// tests/fixtures/attr_target_clones.c
// __attribute__((target_clones(...))) 多版本同函数。
// dcc 解析此属性并存入 FuncDecl.target_clones，
// 当前阶段不实际生成 IFUNC resolver，而是按默认版本编译。

__attribute__((target_clones("default,avx2,avx512f")))
int add3(int a, int b, int c) {
    return a + b + c;
}

int main(void) {
    int r = add3(1, 2, 3);
    if (r == 6) {
        return 0;
    }
    return 1;
}
