/* Test: stack clash protection (-fstack-clash-protection) */

/* Function with large stack frame to trigger page probing */
int large_frame_func(void) {
    char buf[8192];  /* > 1 page (4096 bytes) */
    for (int i = 0; i < 8192; i++) {
        buf[i] = (char)(i & 0xFF);
    }
    return buf[8191];
}

/* Function with very large frame (> 2 pages) */
int very_large_frame(void) {
    char buf[16384];  /* > 4 pages */
    buf[0] = 1;
    buf[16383] = 2;
    return buf[0] + buf[16383];
}

/* Small frame should not trigger probing */
int small_frame(void) {
    int x = 42;
    return x;
}
