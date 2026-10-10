// tests/fixtures/smoke/control_flow.c - 测试 if/while 控制流
int main() {
    int x = 0;
    int i = 0;
    while (i < 10) {
        x = x + i;
        i = i + 1;
    }
    if (x == 45) {
        return 100;   // 1+2+...+9 = 45, 期望 100
    }
    return 0;
}
