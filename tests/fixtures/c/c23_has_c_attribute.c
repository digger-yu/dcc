// C23: __has_c_attribute 属性检测 + __STDC_ENDIAN_* 字节序宏
#include <stdio.h>

#if !__has_c_attribute(fallthrough)
#error "fallthrough must be supported"
#endif
#if !__has_c_attribute(deprecated)
#error "deprecated must be supported"
#endif
#if !__has_c_attribute(maybe_unused)
#error "maybe_unused must be supported"
#endif
#if !__has_c_attribute(nodiscard)
#error "nodiscard must be supported"
#endif
#if __has_c_attribute(__no_such_attribute_xyz__)
#error "__no_such_attribute_xyz__ must not be supported"
#endif

// 属性本体可静默跳过
[[deprecated]] int old_fn(void) { return 1; }
[[nodiscard]] int must_use(void) { return 2; }
[[maybe_unused]] static int unused_var = 3;

#if defined(__STDC_ENDIAN_LITTLE__) && defined(__STDC_ENDIAN_BIG__) && defined(__STDC_ENDIAN_NATIVE__)
// 小端主机: NATIVE == LITTLE
#if __STDC_ENDIAN_LITTLE__ != 1234
#error "__STDC_ENDIAN_LITTLE__ must be 1234"
#endif
#if __STDC_ENDIAN_BIG__ != 4321
#error "__STDC_ENDIAN_BIG__ must be 4321"
#endif
#if __STDC_ENDIAN_NATIVE__ != __STDC_ENDIAN_LITTLE__
#error "host must be little endian"
#endif
#else
#error "C23 endian macros must be defined"
#endif

int main(void) {
    int r = old_fn();
    r += must_use();
    if (r != 3) return 1;
    if (unused_var != 3) return 2;
    printf("OK endian=%d,%d,%d r=%d\n",
           __STDC_ENDIAN_LITTLE__, __STDC_ENDIAN_BIG__,
           __STDC_ENDIAN_NATIVE__, r);
    return 0;
}
