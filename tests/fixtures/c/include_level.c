// Test __INCLUDE_LEVEL__ macro
// __INCLUDE_LEVEL__ should be 0 in the main file and increment for each include level
#include "include_level_inner.h"

int main(void) {
    // In main file, __INCLUDE_LEVEL__ should be 0
#if __INCLUDE_LEVEL__ != 0
    return 1;
#endif
    // Check that the inner header saw level 1
    if (get_inner_level() != 1) return 2;
    return 0;
}
