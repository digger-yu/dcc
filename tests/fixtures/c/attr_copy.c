// tests/fixtures/attr_copy_returns_nonnull.c
// __attribute__((copy(fn))) 产生 fn 的别名；
// __attribute__((returns_nonnull)) 表示函数返回值永远非空。

static int _impl(int x) { return x + 100; }

int my_alias(void) __attribute__((copy(_impl)));
int my_alias(void) { return 0; }

int __attribute__((returns_nonnull)) make_nonzero(int x) {
    if (x == 0) {
        return 1;
    }
    return x;
}

int main(void) {
    int v = make_nonzero(0);
    if (v != 0) {
        return 0;
    }
    return 1;
}
