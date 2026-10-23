/* dcc builtin <stdckdint.h> — ISO C23 7.20 检查整数算术.
 *
 * 与 GCC 一样, 这个头由**编译器**提供而不是 libc: 宿主 gcc < 14 的 include
 * 目录里没有它, 直接搜索会报 E0206。dcc 后端已支持 __builtin_{add,sub,mul}_overflow
 * (x86_64 special/builtins.rs 直接生成溢出检测序列), 这里只把它们包成 C23 宏。
 */
#ifndef __STDCKDINT_H
#define __STDCKDINT_H

#define __STDC_VERSION_STDCKDINT_H__ 202311L

#define ckd_add(r, a, b) ((_Bool)__builtin_add_overflow((a), (b), (r)))
#define ckd_sub(r, a, b) ((_Bool)__builtin_sub_overflow((a), (b), (r)))
#define ckd_mul(r, a, b) ((_Bool)__builtin_mul_overflow((a), (b), (r)))

#endif /* __STDCKDINT_H */
