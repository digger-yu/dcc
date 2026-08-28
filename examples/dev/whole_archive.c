/*
 * whole-archive 测试示例
 *
 * 演示 --whole-archive 用法:
 *   1. 编译 lib 源文件为 .o
 *   2. 打包为 .a 静态库
 *   3. 使用 --whole-archive 强制提取所有成员
 *
 * 编译步骤:
 *   dcc -c -o libfoo.o libfoo.c
 *   ar rcs libfoo.a libfoo.o
 *   dcc -o test main.c -L. --whole-archive -lfoo --no-whole-archive
 *
 * 或通过 -Wl, 形式:
 *   dcc -o test main.c -L. -Wl,--whole-archive -lfoo -Wl,--no-whole-archive
 */

/* libfoo.c: 静态库中的两个函数 */
int foo_add(int a, int b) {
    return a + b;
}

int foo_mul(int a, int b) {
    return a * b;
}

/* 即使 main 只调用 foo_add, --whole-archive 也会把 foo_mul 链接进去 */
