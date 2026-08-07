// 16 字节 ABI 附加测试: float/double 数组、解引用、字段存储路径
#include <stdio.h>

int main(void) {
    int ok = 1;

    // double 数组元素赋值 (Index store 浮点路径)
    double arr[3];
    arr[0] = 1.5;
    arr[1] = 2.5;
    arr[2] = arr[0] + arr[1];
    if (arr[0] != 1.5 || arr[1] != 2.5 || arr[2] != 4.0) {
        printf("FAIL: arr = {%f,%f,%f}\n", arr[0], arr[1], arr[2]);
        ok = 0;
    } else {
        printf("PASS: double arr store/load\n");
    }

    // double* 解引用赋值 (Deref store 浮点路径)
    double val = 0.0;
    double *p = &val;
    *p = 6.5;
    if (val != 6.5) {
        printf("FAIL: *p = %f, expected 6.5\n", val);
        ok = 0;
    } else {
        printf("PASS: double deref store\n");
    }

    // float 字段赋值 (Member store 浮点路径)
    struct F { float x; float y; } fs;
    fs.x = 1.25f;
    fs.y = 2.5f;
    if (fs.x != 1.25f || fs.y != 2.5f) {
        printf("FAIL: fs = {%f,%f}\n", fs.x, fs.y);
        ok = 0;
    } else {
        printf("PASS: float field store\n");
    }

    // int 字段赋值 (回归: 原有 GP 路径)
    struct I { long a; } is;
    is.a = 42;
    if (is.a != 42) {
        printf("FAIL: is.a = %ld\n", is.a);
        ok = 0;
    } else {
        printf("PASS: int field store\n");
    }

    // int 常量赋给 double 字段 (cvtsi2sd 路径)
    struct D { double x; } ds;
    ds.x = 7;
    if (ds.x != 7.0) {
        printf("FAIL: ds.x = %f, expected 7.0\n", ds.x);
        ok = 0;
    } else {
        printf("PASS: int const to double field\n");
    }

    // float 数组元素赋值
    float fa[2];
    fa[0] = 0.5f;
    fa[1] = fa[0] * 3.0f;
    if (fa[0] != 0.5f || fa[1] != 1.5f) {
        printf("FAIL: fa = {%f,%f}\n", fa[0], fa[1]);
        ok = 0;
    } else {
        printf("PASS: float arr store/load\n");
    }

    if (!ok) {
        printf("SOME TESTS FAILED\n");
        return 1;
    }
    printf("ALL TESTS PASSED\n");
    return 0;
}