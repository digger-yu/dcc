// 全局 struct 测试
struct Point { int x; int y; };
struct Point global_pt = {10, 20};

int main() {
    if (global_pt.x != 10) return 1;
    if (global_pt.y != 20) return 2;
    return 0;
}
