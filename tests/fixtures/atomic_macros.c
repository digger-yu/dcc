int main(void) {
    int x = 0;
    atomic_store(&x, 7);
    return atomic_load(&x);
}
