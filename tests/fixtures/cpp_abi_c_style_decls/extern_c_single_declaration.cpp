// [link.spec]: `extern "C"` 的**两种形态**都必须被 C++ 前端接受。
//
// 早前只识别块形态 (`extern "C" { ... }`), 单声明形态 (`extern "C" int f();`)
// 会在 `extern` 之后撞上字符串字面量而解析失败, 整条声明降级成
// `CppDecl::Opaque`, native 后端报 `unsupported opaque declaration`, 整个 TU
// 退回 legacy 文本降级。glibc / GCC 头文件里两种形态是混用的
// (`extern "C" int printf (const char *__restrict __format, ...);`)。
//
// 交叉验证口径: extern "C" 的定义与调用要走到**同一个**未修饰符号 (若 dcc
// 给定义加了 C++ 修饰, 链接期就找不到符号, 或者行为不一致)。
#include <cstdio>

// 单声明形态: 定义
extern "C" int c_add(int a, int b) {
    return a + b;
}

// 单声明形态: 声明 + 稍后定义
extern "C" int c_mul(int a, int b);
extern "C" int c_mul(int a, int b) {
    return a * b;
}

// 单声明形态: **变参声明** (glibc printf 的原型形状), 并在本 TU 给出定义
extern "C" int c_marked(char *buf, unsigned long n, const char *fmt, ...);

extern "C" int c_marked(char *buf, unsigned long n, const char *fmt, ...) {
    // 不依赖 va_arg: 这里只验证「extern "C" + 变参」这一声明形态能编译、
    // 能链接、能被正确调用 (参数个数与顺序都对得上)。
    if (n == 0)
        return -1;
    buf[0] = fmt[0];
    buf[1] = '\0';
    return 2;
}

// 块形态与单声明形态混用
extern "C" {
extern "C" int c_nested();
int c_in_block();
}

// 全局变量也允许 extern "C" 单声明
extern "C" int c_counter;
int c_counter = 0;

static char buf[32];

int main() {
    c_counter = 41;
    printf("add=%d mul=%d\n", c_add(2, 3), c_mul(4, 5));
    printf("nested=%d block=%d\n", c_nested(), c_in_block());
    int n = c_marked(buf, sizeof(buf), "n=%d/%s", c_counter, "ok");
    printf("marked ret=%d buf=%s\n", n, buf);
    printf("counter=%d\n", c_counter);
    return c_add(1, 1) + c_mul(2, 2) - 7;
}

extern "C" int c_nested() {
    return 7;
}

int c_in_block() {
    return 11;
}
