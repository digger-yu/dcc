/* dcc builtin <stdfloat.h> — C23 扩展浮点类型 typedefs.
 *
 * 与 GCC/glibc 一样, 这些名字是 _FloatN / _FloatNx 的别名。dcc 把位宽
 * 退化映射到标准浮点类型 (见 parser.rs 的 _FloatN 映射): _Float16/_Float32
 * -> float, _Float32x/_Float64 -> double, _Float64x/_Float128/_Float128x
 * -> long double。bfloat16_t 按 glibc 约定退化成 uint16_t 存储。
 */
#ifndef __STDFLOAT_H
#define __STDFLOAT_H

typedef _Float16 float16_t;
typedef _Float32 float32_t;
typedef _Float64 float64_t;
typedef _Float128 float128_t;
typedef _Float32x float32x_t;
typedef _Float64x float64x_t;
typedef _Float128x float128x_t;

typedef unsigned short bfloat16_t;

#endif /* __STDFLOAT_H */
