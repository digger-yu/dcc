// C23: typeof_unqual 关键字 (dcc 类型系统不保留限定符, 与 typeof 等价)
#include <stdio.h>

// 类型形式: typeof_unqual(const int) → int
typeof_unqual(const int) gv = 42;

int main(void) {
    // 表达式形式
    typeof_unqual(1) x = 7;
    const int ci = 5;
    typeof_unqual(ci) y = 6;   // 等价 typeof(ci) → int
    typeof(ci) z = 9;          // 对照组

    if (x != 7 || y != 6 || z != 9) return 1;
    if (gv != 42) return 2;

    // 限定符数组: typeof_unqual(const int [3]) → int[3]
    typeof_unqual(const int[3]) arr = {1, 2, 3};
    if (arr[0] != 1 || arr[2] != 3) return 3;

    // 指针类型
    int n = 1;
    typeof_unqual(&n) p = &n;
    if (*p != 1) return 4;

    printf("OK x=%d y=%d z=%d arr=%d,%d,%d *p=%d\n",
           x, y, z, arr[0], arr[1], arr[2], *p);
    return 0;
}
