/* Test: __sync_* legacy atomic operations */

int counter = 0;

void test_fetch_and_ops(void) {
    int old;
    /* __sync_fetch_and_add: returns old value */
    old = __sync_fetch_and_add(&counter, 1);
    (void)old;

    /* __sync_fetch_and_sub: returns old value */
    old = __sync_fetch_and_sub(&counter, 1);
    (void)old;

    /* __sync_fetch_and_or: returns old value */
    old = __sync_fetch_and_or(&counter, 0xFF);
    (void)old;

    /* __sync_fetch_and_and: returns old value */
    old = __sync_fetch_and_and(&counter, 0x0F);
    (void)old;

    /* __sync_fetch_and_xor: returns old value */
    old = __sync_fetch_and_xor(&counter, 0x55);
    (void)old;

    /* __sync_fetch_and_nand: returns old value */
    old = __sync_fetch_and_nand(&counter, 0xFF);
    (void)old;
}

void test_op_and_fetch(void) {
    int new_val;
    /* __sync_add_and_fetch: returns new value */
    new_val = __sync_add_and_fetch(&counter, 1);
    (void)new_val;

    /* __sync_sub_and_fetch: returns new value */
    new_val = __sync_sub_and_fetch(&counter, 1);
    (void)new_val;

    /* __sync_or_and_fetch: returns new value */
    new_val = __sync_or_and_fetch(&counter, 0xFF);
    (void)new_val;

    /* __sync_and_and_fetch: returns new value */
    new_val = __sync_and_and_fetch(&counter, 0x0F);
    (void)new_val;

    /* __sync_xor_and_fetch: returns new value */
    new_val = __sync_xor_and_fetch(&counter, 0x55);
    (void)new_val;

    /* __sync_nand_and_fetch: returns new value */
    new_val = __sync_nand_and_fetch(&counter, 0xFF);
    (void)new_val;
}

void test_compare_and_swap(void) {
    int expected = 0;
    int desired = 42;
    int success;
    int old_val;

    /* __sync_bool_compare_and_swap: returns bool */
    success = __sync_bool_compare_and_swap(&counter, expected, desired);
    (void)success;

    /* __sync_val_compare_and_swap: returns old value */
    old_val = __sync_val_compare_and_swap(&counter, expected, desired);
    (void)old_val;
}

void test_lock_ops(void) {
    int lock = 0;
    int old;

    /* __sync_lock_test_and_set: returns old value */
    old = __sync_lock_test_and_set(&lock, 1);
    (void)old;

    /* __sync_lock_release: releases lock */
    __sync_lock_release(&lock);
}

void test_memory_barrier(void) {
    /* __sync_synchronize: full memory barrier */
    __sync_synchronize();
}

int main(void) {
    test_fetch_and_ops();
    test_op_and_fetch();
    test_compare_and_swap();
    test_lock_ops();
    test_memory_barrier();
    return 0;
}
