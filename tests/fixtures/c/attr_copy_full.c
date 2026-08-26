// Test __attribute__((copy("fn")))
// 创建函数符号别名

#include <stdio.h>

// 原始函数
int original_func(int x) {
    return x * 2 + 1;
}

// copy 属性创建别名
int alias_func(int x) __attribute__((copy("original_func")));

// 另一个例子
static int _internal_impl(int a, int b) {
    return a + b;
}

int public_api(int a, int b) __attribute__((copy("_internal_impl")));

int main() {
    // 通过别名调用
    int r1 = alias_func(10);
    int r2 = public_api(3, 4);
    
    printf("alias_func(10) = %d\n", r1);
    printf("public_api(3, 4) = %d\n", r2);
    
    // 验证结果
    if (r1 != 21) {
        printf("FAIL: alias_func(10) should be 21, got %d\n", r1);
        return 1;
    }
    if (r2 != 7) {
        printf("FAIL: public_api(3, 4) should be 7, got %d\n", r2);
        return 1;
    }
    
    printf("PASS: copy attribute works correctly\n");
    return 0;
}
