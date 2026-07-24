/* wchar_t 函数 codegen (Section 11.3)
 * __builtin_wcslen / wcscpy / wcscmp / wmemcpy / wmemset 等
 * 转发到 libc, 与 __builtin_str* 形式一致.
 * 编译为 -c 后, 验证未定义符号. */
#include <wchar.h>

int test_wcslen(const wchar_t *s) { return __builtin_wcslen(s); }
wchar_t *test_wcscpy(wchar_t *d, const wchar_t *s) { return __builtin_wcscpy(d, s); }
wchar_t *test_wcscat(wchar_t *d, const wchar_t *s) { return __builtin_wcscat(d, s); }
wchar_t *test_wcschr(wchar_t *s, wchar_t c) { return __builtin_wcschr(s, c); }
wchar_t *test_wcsrchr(wchar_t *s, wchar_t c) { return __builtin_wcsrchr(s, c); }
int test_wcscmp(const wchar_t *a, const wchar_t *b) { return __builtin_wcscmp(a, b); }
int test_wcsncmp(const wchar_t *a, const wchar_t *b, size_t n) { return __builtin_wcsncmp(a, b, n); }
wchar_t *test_wmemcpy(wchar_t *d, const wchar_t *s, size_t n) { return __builtin_wmemcpy(d, s, n); }
wchar_t *test_wmemmove(wchar_t *d, const wchar_t *s, size_t n) { return __builtin_wmemmove(d, s, n); }
wchar_t *test_wmemset(wchar_t *s, wchar_t c, size_t n) { return __builtin_wmemset(s, c, n); }
int test_wmemcmp(const wchar_t *a, const wchar_t *b, size_t n) { return __builtin_wmemcmp(a, b, n); }
