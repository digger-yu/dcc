// Test __TIMESTAMP__ macro
// __TIMESTAMP__ should expand to a string representing the last modification time
// of the source file. Format: "Day Mon DD HH:MM:SS YYYY"
int main(void) {
    const char *ts = __TIMESTAMP__;
    // __TIMESTAMP__ should be defined and non-empty
    if (ts == 0) return 1;
    if (ts[0] == '\0') return 2;
    // Should be at least 20 characters (e.g., "Mon Jan  1 00:00:00 2024")
    int len = 0;
    const char *p = ts;
    while (*p) {
        len++;
        p++;
    }
    if (len < 20) return 3;
    // Should contain a space (format has spaces)
    p = ts;
    while (*p) {
        if (*p == ' ') return 0;
        p++;
    }
    return 4;
}
