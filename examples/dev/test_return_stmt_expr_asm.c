int main(void) {
    return ({
        int success;
        asm volatile("" : [cc] "=qm" (success) : : "memory");
        success;
    });
}
