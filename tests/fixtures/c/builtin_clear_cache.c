/* Test __builtin_clear_cache and __builtin___clear_cache */

int main(void) {
    char code[64];
    /* Both should compile and link without error */
    __builtin___clear_cache(code, code + 64);
    __builtin_clear_cache(code, code + 64);
    return 0;
}
