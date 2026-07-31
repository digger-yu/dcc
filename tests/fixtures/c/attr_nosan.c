// tests/fixtures/attr_no_sanitize.c
// __attribute__((no_sanitize(...))) 应被解析并存入 FuncDecl.no_sanitize。
// sanitizer 插桩会跳过带此属性的函数。

__attribute__((no_sanitize("address")))
int hot_path(int x) {
    return x * 2;
}

__attribute__((no_sanitize_address))
int legacy(int x) {
    return x + 1;
}

int main(void) {
    int a = hot_path(21);
    int b = legacy(41);
    if (a == 42 && b == 42) {
        return 0;
    }
    return 1;
}
