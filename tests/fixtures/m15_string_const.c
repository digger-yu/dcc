// 字符串字面量作为常量 + 拼接 (M15)
extern int printf(const char *, ...);

int main(void) {
    // 字符串拼接: "a" "b" "c" 合并为 "abc"
    const char *s = "Hello, " "World" "!";
    printf("%s\n", s);
    return 0;
}
