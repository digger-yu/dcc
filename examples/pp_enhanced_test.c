// 测试预处理器增强特性 (期望返回 42)
int main() {
    int result = 0;

    // 1. GCC 兼容宏
#if defined(__GNUC__) && __GNUC__ >= 10
    result += 1;
#endif

    // 2. 三元运算符
#if (1 ? 2 : 0) == 2
    result += 2;
#endif

    // 3. 位运算
#if (0xFF & 0x0F) == 0x0F
    result += 4;
#endif

#if (0xF0 | 0x0F) == 0xFF
    result += 5;
#endif

#if (0xFF ^ 0x0F) == 0xF0
    result += 6;
#endif

    // 4. 移位运算
#if (1 << 3) == 8
    result += 7;
#endif

#if (16 >> 2) == 4
    result += 8;
#endif

    // 5. 字符常量
#if 'A' == 65
    result += 9;
#endif

    // 结果: 1+2+4+5+6+7+8+9 = 42
    return result;
}
