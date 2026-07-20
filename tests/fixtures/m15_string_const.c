// 字符串字面量拼接 (M15)
int main(void) {
    // "a" "b" "c" 合并为 "abc" (3 字符)
    const char *s = "a" "b" "c";
    // 用 \0 终止符的位置反推长度
    int n = 0;
    while (s[n]) n++;
    return n == 3 ? 0 : 1;
}
