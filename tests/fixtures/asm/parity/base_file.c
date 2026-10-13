// Test __BASE_FILE__ macro
// __BASE_FILE__ should expand to the name of the main source file
int main(void) {
    const char *base = __BASE_FILE__;
    // __BASE_FILE__ should be defined and non-empty
    if (base == 0) return 1;
    if (base[0] == '\0') return 2;
    // Should have at least one character
    return 0;
}
