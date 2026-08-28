/* Test: C99 <inttypes.h> format macros, <float.h>, <limits.h>, <errno.h> */

/* inttypes.h format macros (via __PRId64 etc. builtins) */
/* These are used by system headers to define PRId64, PRIu64 etc. */

/* float.h macros */
#if __FLT_RADIX__ != 2
#error "FLT_RADIX should be 2"
#endif

#if __DBL_MANT_DIG__ != 53
#error "DBL_MANT_DIG should be 53"
#endif

#if __FLT_DIG__ != 6
#error "FLT_DIG should be 6"
#endif

/* limits.h macros */
#if __CHAR_BIT__ != 8
#error "CHAR_BIT should be 8"
#endif

#if __INT_MAX__ != 2147483647
#error "INT_MAX should be 2147483647"
#endif

#if __LONG_LONG_MAX__ != 9223372036854775807LL
#error "LLONG_MAX should be 9223372036854775807LL"
#endif

/* errno.h constants */
#if EDOM != 33
#error "EDOM should be 33"
#endif

#if ERANGE != 34
#error "ERANGE should be 34"
#endif

int main(void) {
    return 0;
}
