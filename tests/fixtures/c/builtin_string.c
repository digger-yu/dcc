/* Test: missing __builtin_* string/memory functions */

/* __builtin_mempcpy: copy and return dst+n */
void test_mempcpy(void) {
    char src[] = "hello";
    char dst[10];
    char *end = __builtin_mempcpy(dst, src, 5);
    (void)end;
}

/* __builtin_stpcpy: copy string, return pointer to end */
void test_stpcpy(void) {
    char dst[20];
    char *end = __builtin_stpcpy(dst, "world");
    (void)end;
}

/* __builtin_strnlen: bounded string length */
void test_strnlen(void) {
    const char *s = "test";
    size_t len = __builtin_strnlen(s, 10);
    (void)len;
}

/* __builtin_strspn: span of characters */
void test_strspn(void) {
    const char *s = "abcdef";
    const char *accept = "abc";
    size_t span = __builtin_strspn(s, accept);
    (void)span;
}

/* __builtin_strcspn: complement span */
void test_strcspn(void) {
    const char *s = "abcdef";
    const char *reject = "de";
    size_t span = __builtin_strcspn(s, reject);
    (void)span;
}

/* __builtin_strcasestr: case-insensitive substring search */
void test_strcasestr(void) {
    const char *haystack = "Hello World";
    const char *needle = "WORLD";
    char *found = __builtin_strcasestr(haystack, needle);
    (void)found;
}

/* __builtin_strncmp: bounded string comparison */
void test_strncmp(void) {
    const char *s1 = "abc";
    const char *s2 = "abd";
    int cmp = __builtin_strncmp(s1, s2, 3);
    (void)cmp;
}

int main(void) {
    test_mempcpy();
    test_stpcpy();
    test_strnlen();
    test_strspn();
    test_strcspn();
    test_strcasestr();
    test_strncmp();
    return 0;
}
