/* Test: C11/C23 standard library compliance macros */

/* Unicode support declarations */
#ifndef __STDC_UTF_16__
#error "__STDC_UTF_16__ not defined"
#endif
#ifndef __STDC_UTF_32__
#error "__STDC_UTF_32__ not defined"
#endif

/* wchar_t Unicode (ISO 10646) declaration */
#ifndef __STDC_ISO_10646__
#error "__STDC_ISO_10646__ not defined"
#endif

/* offsetof macro via __builtin_offsetof */
struct test_off { int a; int b; char c; };
int off = offsetof(struct test_off, b);

/* Type limit macros exist (already defined, verify) */
#ifndef __WCHAR_TYPE__
#error "__WCHAR_TYPE__ not defined"
#endif
#ifndef __SIZE_TYPE__
#error "__SIZE_TYPE__ not defined"
#endif
#ifndef __PTRDIFF_TYPE__
#error "__PTRDIFF_TYPE__ not defined"
#endif

int main(void) { return 0; }
