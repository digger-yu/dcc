// C23: printf %b/%B 二进制格式 (需 libc 支持; 验证 dcc 格式检查放行)
#include <stdio.h>

int main(void) {
    unsigned int v = 5;   // 0b101
    // %b 小写格式, %B 大写格式 (C23 7.21.6.1)
    printf("%b %B\n", v, v);
    printf("%#b %#B\n", v, v);     // 0b 前缀
    printf("%08b\n", v);           // 宽度补零
    printf("%b\n", 0u);
    printf("%u\n", 123u);
    return 0;
}
