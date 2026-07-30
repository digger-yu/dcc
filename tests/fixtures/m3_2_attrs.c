/* m3_2: __attribute__ series - packed, aligned, section, weak,
 * alias, visibility, noreturn, unused */
#include <stddef.h>

/* --- packed --- */
struct __attribute__((packed)) Packed {
    char a;
    int b;
    char c;
};

/* --- aligned --- */
struct __attribute__((aligned(16))) Align16 {
    int x;
};

/* --- section --- */
int my_var __attribute__((section(".mydata"))) = 0x1234;

/* --- weak --- */
int weak_var2 __attribute__((weak)) = 88;

/* --- alias --- */
int real_func(void) { return 77; }
int alias_func(void) __attribute__((alias("real_func")));

/* --- visibility --- */
int hidden_var __attribute__((visibility("hidden"))) = 99;
int hidden_fn(void) __attribute__((visibility("hidden")));
int hidden_fn(void) { return 55; }

/* --- unused --- */
int main(void) {
    int fail = 0;
    int __attribute__((unused)) unused_local = 42;

    /* packed: sizeof = 6 (1+4+1) */
    if (sizeof(struct Packed) != 6) fail |= 1;

    /* packed: offsetof(b) = 1 */
    if (offsetof(struct Packed, b) != 1) fail |= 2;

    /* aligned(16): alignof = 16 */
    if (_Alignof(struct Align16) != 16) fail |= 4;

    /* aligned(16): sizeof % 16 == 0 */
    if (sizeof(struct Align16) % 16 != 0) fail |= 8;

    /* section: variable is accessible */
    if (my_var != 0x1234) fail |= 16;

    /* weak: weak_var2 is accessible */
    if (weak_var2 != 88) fail |= 32;

    /* alias: alias_func() should call real_func() → 77 */
    if (alias_func() != 77) fail |= 64;

    /* visibility: hidden_var accessible */
    if (hidden_var != 99) fail |= 128;

    /* visibility: hidden_fn accessible */
    if (hidden_fn() != 55) fail |= 256;

    return fail;
}
