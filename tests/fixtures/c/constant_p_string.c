/* tests/fixtures/c/constant_p_string.c
 *
 * `__builtin_constant_p` 对**字符串字面量**必须给 1：字面量是一个静态对象的地址，是
 * 链接期常量（gcc 在此折 1）。旧实现复用「能不能算出一个 i128」那条判据，字符串过不去，
 * 于是返回 0；gcc-torture 的 `bcp-1.c` 的 `good1` 正是这一条。
 *
 * 同时留下反向对照：可写局部数组与 `&全局` 都**不是**编译期常量（必须为 0），
 * 避免把判据放宽成「一切皆常量」。
 */
int global;

int main(void) {
    char buf[16];

    if (!__builtin_constant_p("hi")) {
        return 1;
    }
    if (!__builtin_constant_p(1 + 2)) {
        return 2;
    }
    if (__builtin_constant_p(buf)) {
        return 3;
    }
    if (__builtin_constant_p(&global)) {
        return 4;
    }
    if (__builtin_constant_p(global)) {
        return 5;
    }
    return 0;
}
