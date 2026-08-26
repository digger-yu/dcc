// Inner header for __INCLUDE_LEVEL__ test
// Should see __INCLUDE_LEVEL__ == 1 when included from main file
static inline int get_inner_level(void) {
#if __INCLUDE_LEVEL__ != 1
    return -1;
#else
    return __INCLUDE_LEVEL__;
#endif
}
