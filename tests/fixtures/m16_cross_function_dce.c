// m16_cross_function_dce.c
// 跨函数 DCE: 内部 static unused_func 应当被删除
// 通过: exit 0

static int unused_func(int x) {
    return x * 2 + 100;
}

int keep_this(int x) {
    return x + 1;
}

int main() {
    int r = keep_this(41);
    if (r != 42) return 1;
    return 0;
}
