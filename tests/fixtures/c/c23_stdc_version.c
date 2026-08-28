/* Test: __STDC_VERSION__ for C23 should be 202312L */
#if defined(__STDC_VERSION__)
#if __STDC_VERSION__ != 202312L
#error "__STDC_VERSION__ should be 202312L for C23"
#endif
int version_correct = 1;
#else
#error "__STDC_VERSION__ should be defined in C23 mode"
#endif
