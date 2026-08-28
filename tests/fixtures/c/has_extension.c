// tests/fixtures/c/has_extension.c
// __has_extension: Clang 兼容特性检测宏.
// 语义等价于 __has_feature, 但额外包含已成为标准基线的特性.

// 已知特性应返回 1
#if !__has_extension(c_atomic)
#error "c_atomic should be supported"
#endif

#if !__has_extension(tls)
#error "tls should be supported"
#endif

#if !__has_extension(c_alignas)
#error "c_alignas should be supported"
#endif

#if !__has_extension(c23_constexpr)
#error "c23_constexpr should be supported"
#endif

#if !__has_extension(int128)
#error "int128 should be supported"
#endif

// 未知特性应返回 0
#if __has_extension(nonexistent_feature_xyz)
#error "nonexistent_feature_xyz should not be supported"
#endif

// __has_extension 可在宏展开上下文中使用
#define HAS_EXT(x) __has_extension(x)
#if !HAS_EXT(c_static_assert)
#error "c_static_assert should be supported via macro"
#endif

int main(void) {
    return 0;
}
