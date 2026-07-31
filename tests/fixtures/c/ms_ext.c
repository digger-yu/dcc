// tests/fixtures/ms_extensions.c
// MSVC 扩展关键字：_ms_restrict / __declspec 应被静默接受，不影响语义。

__declspec(noreturn) void die(void) {
    for (;;) {
    }
}

__declspec(thread) int tls_var = 0;

__declspec(align(16)) struct S {
    int x;
};

int copy_buf(int * _ms_restrict dst, const int * _ms_restrict src, int n) {
    for (int i = 0; i < n; i++) {
        dst[i] = src[i];
    }
    return n;
}

int main(void) {
    int a[4] = {1, 2, 3, 4};
    int b[4] = {0};
    int n = copy_buf(b, a, 4);
    tls_var = n;
    if (b[3] == 4) {
        return 0;
    }
    return 1;
}
