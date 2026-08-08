// tests/fixtures/c/c23_constexpr_static.c
// C23 constexpr 与 static 组合: 解析为 static inline + always_inline.

static constexpr int triple(int x) {
    return x * 3;
}

constexpr static int quad(int x) {
    return x * 4;
}

int main(void) {
    return (triple(7) == 21 && quad(5) == 20) ? 0 : 1;
}
