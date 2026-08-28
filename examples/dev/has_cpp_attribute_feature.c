/* Test: __has_cpp_attribute and __has_feature */

/* __has_cpp_attribute - known attributes return version numbers */
#if __has_cpp_attribute(nodiscard) > 0
int has_nodiscard;
#endif

#if __has_cpp_attribute(maybe_unused) > 0
int has_maybe_unused;
#endif

#if __has_cpp_attribute(fallthrough) > 0
int has_fallthrough;
#endif

/* __has_cpp_attribute - unknown attribute returns 0 */
#if __has_cpp_attribute(totally_unknown) == 0
int unknown_attr_zero;
#endif

/* __has_feature - known features */
#if __has_feature(c_atomic)
int has_c_atomic;
#endif

#if __has_feature(tls)
int has_tls;
#endif

#if __has_feature(builtin_add_overflow)
int has_overflow;
#endif

/* __has_feature - unknown feature returns 0 */
#if __has_feature(nonexistent_feature) == 0
int unknown_feature_zero;
#endif
