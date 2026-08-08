// tests/fixtures/ms_extensions.c
// MSVC 扩展关键字：_ms_restrict / __declspec / __pragma 应被静默接受，不影响语义。

__declspec(noreturn) void die(void) {
    for (;;) {
    }
}

__declspec(thread) int tls_var = 0;

__declspec(align(16)) struct S {
    int x;
};

// dllimport / dllexport: 接受但无 codegen 差异
__declspec(dllimport) int ext_imported_var;
__declspec(dllexport) int ext_exported_var = 42;
__declspec(dllimport) void ext_imported_fn(void);

// __pragma: 无操作
__pragma(message("compiling ms_ext.c"))
__pragma(pack(push, 1))
__pragma(pack(pop))

int copy_buf(int * _ms_restrict dst, const int * _ms_restrict src, int n) {
    // __pragma 在函数体内也应被接受
    __pragma(message("in function"))
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
    ext_exported_var = n;
    if (b[3] == 4) {
        return 0;
    }
    return 1;
}
