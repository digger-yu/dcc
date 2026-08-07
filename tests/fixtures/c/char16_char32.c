// char16_t / char32_t (C11/C++11) 基本测试
int main(void) {
    char16_t c16 = u'A';
    char32_t c32 = U'B';
    
    // 字符值验证
    if (c16 != 65) return 3;   // 'A' = 65
    if (c32 != 66) return 4;   // 'B' = 66
    
    return 0;
}
