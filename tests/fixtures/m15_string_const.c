// 字符串字面量拼接 (M15) - parser 层完成, 测试 const 字符串拼接
int main(void) {
    // "a" "b" "c" 合并为 "abc" (3 字符 + NUL 终止)
    char s[4] = "a" "b" "c";
    int n = 0;
    while (s[n]) n++;
    return n == 3 ? 0 : 1;
}
