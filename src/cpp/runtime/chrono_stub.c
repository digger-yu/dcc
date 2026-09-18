// Chrono runtime stubs for DCC
// Minimal implementations to satisfy std::chrono headers

#include <stddef.h>

void dccpp_dur_free(void *dur) {
    // Stub: duration destructor is a no-op
}

void dccpp_tp_free(void *tp) {
    // Stub: time_point destructor is a no-op
}

void *dccpp_sysclock_now(void) {
    // Stub: return dummy pointer for system_clock::now()
    return NULL;
}

void *dccpp_steadyclock_now(void) {
    // Stub: return dummy pointer for steady_clock::now()
    return NULL;
}

void *dccpp_hrclock_now(void) {
    // Stub: return dummy pointer for high_resolution_clock::now()
    return NULL;
}
