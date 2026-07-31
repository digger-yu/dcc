// 综合初始化器测试
int global_arr[3] = {1, 2, 3};

struct Point { int x; int y; };
struct Point global_pt = {10, 20};

int main() {
    // 1. 数组初始化
    int arr[3] = {10, 20, 30};
    if (arr[0] != 10) return 1;
    if (arr[1] != 20) return 2;
    if (arr[2] != 30) return 3;

    // 2. 全局数组
    if (global_arr[0] != 1) return 4;
    if (global_arr[1] != 2) return 5;
    if (global_arr[2] != 3) return 6;

    // 3. struct 初始化
    struct Point p = {100, 200};
    if (p.x != 100) return 7;
    if (p.y != 200) return 8;

    // 4. 全局 struct
    if (global_pt.x != 10) return 9;
    if (global_pt.y != 20) return 10;

    // 5. 单元素
    int x = {42};
    if (x != 42) return 11;

    return 0;
}
