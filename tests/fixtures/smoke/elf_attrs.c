/* Phase 2: inspect with readelf -SWsWr and run after linking with gcc. */
int global_value = 7;

int *custom_pointer
    __attribute__((section(".data.custom"), aligned(16))) = &global_value;

int custom_function(void)
    __attribute__((section(".init.text"), aligned(32))) {
    return *custom_pointer;
}

__attribute__((weak)) int weak_function(void) {
    return 1;
}

__attribute__((visibility("hidden"), aligned(64))) int hidden_bss;

int main(void) {
    return custom_function() == 7 && weak_function() == 1 ? 0 : 1;
}
