int main(void) {
    int success;
    asm volatile("" : [cc] "=qm" (success) : : "memory");
    return success;
}
