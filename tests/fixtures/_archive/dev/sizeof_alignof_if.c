/* Test: sizeof / __alignof__ / __builtin_offsetof in #if */

/* sizeof basic types */
#if sizeof(int) == 4
int sizeof_int_ok;
#else
#error "sizeof(int) should be 4"
#endif

#if sizeof(char) == 1
int sizeof_char_ok;
#else
#error "sizeof(char) should be 1"
#endif

#if sizeof(long long) == 8
int sizeof_ll_ok;
#else
#error "sizeof(long long) should be 8"
#endif

#if sizeof(void*) == 8
int sizeof_ptr_ok;
#else
#error "sizeof(void*) should be 8 on x86_64"
#endif

/* __alignof__ */
#if __alignof__(int) == 4
int alignof_int_ok;
#else
#error "__alignof__(int) should be 4"
#endif

#if __alignof__(double) == 8
int alignof_double_ok;
#else
#error "__alignof__(double) should be 8"
#endif

/* __builtin_offsetof (returns 0 in preprocessor) */
#if __builtin_offsetof(struct any, member) == 0
int offsetof_ok;
#else
#error "__builtin_offsetof should be 0 in preprocessor"
#endif
