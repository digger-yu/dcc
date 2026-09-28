/* Test atomic builtins - isolate segfault */
#define __ATOMIC_RELAXED 0
#define __ATOMIC_ACQUIRE 2
#define __ATOMIC_RELEASE 3
#define __ATOMIC_SEQ_CST  5

static int counter;
static int expected_val;
static int desired_val;

int main(void) {
    /* 1. fetch_add */
    counter = 0;
    if (__atomic_fetch_add(&counter, 10, __ATOMIC_RELAXED) != 0) return 1;

    /* 2. store/load */
    __atomic_store_n(&counter, 42, __ATOMIC_RELEASE);
    if (__atomic_load_n(&counter, __ATOMIC_ACQUIRE) != 42) return 2;

    /* 3. compare_exchange_n */
    counter = 10;
    expected_val = 10;
    if (!__atomic_compare_exchange_n(&counter, &expected_val, 20,
            0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) return 3;

    /* 4. compare_exchange (non-_n) */
    counter = 100;
    expected_val = 100;
    desired_val = 200;
    if (!__atomic_compare_exchange(&counter, &expected_val, &desired_val,
            0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) return 4;

    /* 5. exchange */
    counter = 50;
    if (__atomic_exchange_n(&counter, 60, __ATOMIC_SEQ_CST) != 50) return 5;

    return 0;
}
