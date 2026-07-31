// C23: nullptr_t 内置类型 + nullptr 关键字
#include <stdio.h>

int main(void) {
    // nullptr_t 类型对象
    nullptr_t np = nullptr;
    if (np != 0) return 1;

    // nullptr_t 与指针类型互相转换
    int *p = np;
    if (p != 0) return 2;
    nullptr_t back = p;
    if (back != 0) return 3;

    // nullptr_t 大小 = void*
    if (sizeof(nullptr_t) != sizeof(void *)) return 4;

    // nullptr 是唯一值
    nullptr_t n2 = nullptr;
    if (n2 != np) return 5;

    // 函数参数/返回值
    nullptr_t id(void) { return nullptr; }
    if (id() != 0) return 6;

    // 条件判断
    if (np) return 7;
    if (!np == 0) return 8;

    // nullptr 本身不是指针, 但可赋值/比较
    int *q = nullptr;
    if (q != nullptr) return 9;

    printf("OK np=%d p=%d back=%d sizeof=%zu\n",
           (int)(np != 0), (int)(p != 0), (int)(back != 0), sizeof(nullptr_t));
    return 0;
}
