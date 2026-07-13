// C11 full test - all inline in main, simple exit codes 0-13
_Static_assert(1, "ok");
_Static_assert(sizeof(int) == 4, "int size");

_Atomic(int) g_atomic;

struct SA { int a; _Atomic(int) b; };

struct AU { int x; union { int i; float f; }; int y; };

union AS { int a; struct { short low; short high; }; };

struct CX { int x; union { int i; float f; }; _Atomic(int) counter; };

int main(void) {
    // Test 1: _Atomic global assign/return
    g_atomic = 42;
    if (g_atomic != 42) return 1;

    // Test 2: struct with _Atomic field
    struct SA s;
    s.a = 1;
    s.b = 42;
    if (s.b != 42) return 2;

    // Test 3: anonymous union
    struct AU u;
    u.x = 10;
    u.i = 42;
    u.y = 20;
    if (u.i != 42) return 3;

    // Test 4: anonymous struct in union
    union AS a;
    a.low = 10;
    a.high = 20;
    if (a.low != 10) return 4;

    // Test 5: __atomic_fetch_add (int)
    int x5 = 0;
    int r5 = __atomic_fetch_add(&x5, 1, 5);
    if (r5 != 0) return 5;
    if (x5 != 1) return 51;

    // Test 6: __atomic_fetch_sub (int)
    int x6 = 10;
    __atomic_fetch_sub(&x6, 3, 5);
    if (x6 != 7) return 6;

    // Test 7: __atomic_exchange_n (int)
    int x7 = 10;
    int r7 = __atomic_exchange_n(&x7, 99, 5);
    if (r7 != 10) return 7;
    if (x7 != 99) return 71;

    // Test 8: __atomic_compare_exchange_n (int)
    int x8 = 10;
    int exp8 = 10;
    int ok8 = __atomic_compare_exchange_n(&x8, &exp8, 42, 0, 5, 5);
    if (ok8 != 1) return 8;
    if (x8 != 42) return 81;

    // Test 9: __atomic_fetch_and (int)
    int x9 = 0xFF;
    __atomic_fetch_and(&x9, 0xF0, 5);
    if (x9 != 0xF0) return 9;

    // Test 10: __atomic_fetch_or (int)
    int x10 = 0x0F;
    __atomic_fetch_or(&x10, 0xF0, 5);
    if (x10 != 0xFF) return 10;

    // Test 11: __atomic_fetch_xor (int)
    int x11 = 0xFF;
    __atomic_fetch_xor(&x11, 0x0F, 5);
    if (x11 != 0xF0) return 11;

    // Test 12: _Atomic(int) local var
    _Atomic(int) x12 = 0;
    x12 = 42;
    if (x12 != 42) return 12;

    // Test 13: Combined: struct + anonymous union + _Atomic field + fetch_add
    struct CX c;
    c.x = 10;
    c.i = 20;
    c.counter = 42;
    __atomic_fetch_add(&c.counter, 1, 5);
    if (c.counter != 43) return 13;

    // All tests passed
    return 0;
}
