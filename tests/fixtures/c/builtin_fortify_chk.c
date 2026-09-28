/* m27_fortify_chk.c - Linux FORTIFY __builtin___*_chk 链接行为测试 */
/* 当 _FORTIFY_SOURCE > 0 时, 系统头文件展开 memcpy 等为 __builtin___memcpy_chk */
/* 编译器应剥离 __builtin_ 前缀, 链接到 libc 的 __*_chk 符号 */

/* 手工模拟 FORTIFY 宏展开: 直接调用 __builtin___memcpy_chk */
extern void *__builtin___memcpy_chk(void *dest, const void *src, unsigned long n, unsigned long bos);

int main(void) {
    char buf[64];
    char src[] = "he";
    /* 直接调用 FORTIFY builtin, 应链接到 __memcpy_chk */
    /* n=2, bos=64, 不会触发溢出检查 */
    __builtin___memcpy_chk(buf, src, 2, 64);
    /* 简单验证: 检查 buf 内容 */
    if (buf[0] != 'h' || buf[1] != 'e') {
        return 1;
    }
    return 0;
}