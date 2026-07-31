// C23: 省略 '=' 的大括号初始化 int x{5} / int a[]{1,2,3}
#include <stdio.h>

struct P { int x; int y; };

// 全局: 标量与数组
int g{5};
int garr[]{10, 20, 30};

int main(void) {
    // 标量大括号初始化
    int x{5};
    int y = {6};           // 传统形式对照
    if (x != 5 || y != 6) return 1;

    // 数组 (省略 =)
    int arr[]{1, 2, 3};
    if (arr[0] != 1 || arr[1] != 2 || arr[2] != 3) return 2;

    // 指定大小数组
    int arr4[4]{7, 8};
    if (arr4[0] != 7 || arr4[1] != 8 || arr4[2] != 0 || arr4[3] != 0) return 3;

    // 结构体 (省略 =)
    struct P p{1, 2};
    if (p.x != 1 || p.y != 2) return 4;

    // 嵌套
    int m[2][2]{{1, 2}, {3, 4}};
    if (m[0][1] != 2 || m[1][0] != 3) return 5;

    // 全局
    if (g != 5) return 6;
    if (garr[0] != 10 || garr[2] != 30) return 7;

    // char / 指针标量
    char c{'A'};
    int *ptr{0};
    if (c != 'A' || ptr != 0) return 8;

    printf("OK x=%d y=%d arr=%d,%d,%d p=%d,%d g=%d garr=%d,%d,%d c=%c\n",
           x, y, arr[0], arr[1], arr[2], p.x, p.y, g, garr[0], garr[1], garr[2], c);
    return 0;
}
