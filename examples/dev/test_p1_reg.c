int main(void) {
    register int rx __asm__("r10");
    rx = 42;
    if (rx != 42) return 1;
    return 0;
}
