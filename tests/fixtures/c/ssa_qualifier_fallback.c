/* Test: IR->SSA qualifier fallback for switch/label declarations
 *
 * Variables declared inside switch cases and after labels should
 * retain their qualifiers (volatile, const) when the compiler
 * goes through the IR->SSA->AST rebuild path.
 */

int test_switch_volatile(int x) {
    int result = 0;
    switch (x) {
    case 1: {
        volatile int v = 42;
        result = v;
        break;
    }
    case 2: {
        volatile int v = 100;
        result = v + 1;
        break;
    }
    default: {
        int n = 0;
        result = n;
        break;
    }
    }
    return result;
}

int test_label_volatile(int x) {
    volatile int flag = 1;
    if (x > 0) goto positive;
    return 0;
positive:
    volatile int val = x + flag;
    return val;
}

int main(void) {
    int ok = 1;
    ok &= (test_switch_volatile(1) == 42);
    ok &= (test_switch_volatile(2) == 101);
    ok &= (test_switch_volatile(99) == 0);
    ok &= (test_label_volatile(5) == 6);
    ok &= (test_label_volatile(-1) == 0);
    return ok ? 0 : 1;
}
