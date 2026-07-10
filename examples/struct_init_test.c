// 测试 struct 初始化
struct Point { int x; int y; };

int main() {
    struct Point p = {100, 200};
    if (p.x != 100) return 1;
    if (p.y != 200) return 2;
    return 0;
}
