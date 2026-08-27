// Test fixture for __builtin_prefetch instruction generation
void prefetch_read_l1(const void *addr) {
    __builtin_prefetch(addr, 0, 3);  // prefetcht0
}

void prefetch_read_nta(const void *addr) {
    __builtin_prefetch(addr, 0, 0);  // prefetchnta
}

void prefetch_write(void *addr) {
    __builtin_prefetch(addr, 1, 3);  // prefetchw
}

void prefetch_default(const void *addr) {
    __builtin_prefetch(addr);  // default: read, locality=3 -> prefetcht0
}
