/* Test: C89 multi-character character constants */
/* 'ab' form: implementation-defined, GCC uses big-endian concatenation */

/* Basic multi-character constants */
int a = 'ab';       /* ('a' << 8) | 'b' = 0x6162 */
int b = 'abc';      /* ('a' << 16) | ('b' << 8) | 'c' = 0x616263 */
int c = 'abcd';     /* 0x61626364 */

/* Single character (should still work) */
int d = 'x';        /* 0x78 */

/* Multi-char with escape sequences */
int e = 'a\n';      /* ('a' << 8) | 0x0a = 0x610a */
int f = '\t b';     /* (0x09 << 16) | (' ' << 8) | 'b' */

/* Multi-char constants have type int */
int arr['AB' == 0x4142 ? 1 : 0]; /* should be size 1 */

/* Verify value at compile time */
_Static_assert('ab' == 0x6162, "multi-char constant value");
_Static_assert('a' == 0x61, "single char value");

int main(void) {
    return 0;
}
