// extended_float_types.cpp — 扩展浮点内建类型 (_Float32/_Float32x/...) 正向夹具
//
// glibc `<bits/floatn-common.h>` / `math.h` 以 `typedef _Float32 float_t;` /
// `typedef _Float32x double_t;` 形态使用这些 TS18661 类型名; 当宿主编译器报
// `__GNUC__ >= 13` 时, `typedef float _Float32;` 那类定义被 `__GNUC_PREREQ`
// 守卫跳过 (真 GCC 把它们当内建关键字), 只留下使用点 —— native 后端必须自己
// 认识这些类型名, 否则整份含 <math.h> 的 TU 报 `unknown class type: _Float32`
// (libcpp 缺陷链上的一站)。
//
// x86_64 glibc 的格式等价: _Float32=float(32), _Float32x=double(64)。
#include <cstdio>

typedef _Float32 my_f32;
typedef _Float32x my_f32x;

int main() {
    my_f32 a = 1.5f;
    my_f32x b = 2.25;
    // 尺寸与真实 float/double 一致 (证明宽度映射正确, 而非退化成 1)。
    printf("size_f32=%d\n", (int)sizeof(my_f32));
    printf("size_f32x=%d\n", (int)sizeof(my_f32x));
    printf("a=%d b=%d sum=%d\n", (int)a, (int)b, (int)(a + (my_f32)b));
    return 0;
}
