// C23 __has_* 家族 + __STDC_ENDIAN_* 端到端 (M26)
#if !__has_include(<stdio.h>)
#error "__has_include(<stdio.h>) should be true"
#endif
#if __has_include("dcc-no-such-header-xyz.h")
#error "__has_include of missing header should be false"
#endif
#if !__has_c_attribute(fallthrough)
#error "__has_c_attribute(fallthrough) should be true"
#endif
#if !__has_c_attribute(deprecated)
#error "__has_c_attribute(deprecated) should be true"
#endif
#if __has_c_attribute(dcc_no_such_attr)
#error "__has_c_attribute of unknown attr should be false"
#endif
#if !__has_builtin(__builtin_expect)
#error "__has_builtin(__builtin_expect) should be true"
#endif
#if !__has_attribute(packed)
#error "__has_attribute(packed) should be true"
#endif
#if !__has_attribute(aligned)
#error "__has_attribute(aligned) should be true"
#endif
int main(void) {
    // 小端主机: NATIVE 必须等于 LITTLE
#if __STDC_ENDIAN_NATIVE__ == __STDC_ENDIAN_LITTLE__
    return 0;
#elif __STDC_ENDIAN_NATIVE__ == __STDC_ENDIAN_BIG__
    return 2;
#else
    return 3;
#endif
}
