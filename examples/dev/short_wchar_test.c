/* Test: -fshort-wchar (wchar_t 2-byte mode) */

/* Wide character literal */
wchar_t wc = L'A';

/* Wide string literal */
const wchar_t *ws = L"Hello";

/* Check wchar_t size via preprocessor */
#if defined(__WCHAR_TYPE__)
/* __WCHAR_TYPE__ should be "short unsigned int" with -fshort-wchar */
#endif

int check_wchar(void) {
    wchar_t local = L'X';
    const wchar_t *str = L"test";
    (void)local;
    (void)str;
    return sizeof(wchar_t);  /* Should be 2 with -fshort-wchar */
}
