// 测试 __has_builtin 返回正确值

#if __has_builtin(__builtin_expect)
int has_expect = 1;
#else
int has_expect = 0;
#endif

#if __has_builtin(__builtin_memcpy)
int has_memcpy = 1;
#else
int has_memcpy = 0;
#endif

#if __has_builtin(__builtin_nonexistent)
int has_nonexistent = 1;
#else
int has_nonexistent = 0;
#endif

#if __has_attribute(packed)
int has_packed = 1;
#else
int has_packed = 0;
#endif

int main() {
    if (!has_expect) return 1;
    if (!has_memcpy) return 2;
    if (has_nonexistent) return 3;
    if (!has_packed) return 4;
    return 0;
}
