/* Test: __attribute__((mode(...))) type mode mapping */

/* Integer modes: QI=8bit, HI=16bit, SI=32bit, DI=64bit */
typedef int __attribute__((mode(__QI__))) int8_mode_t;
typedef unsigned int __attribute__((mode(__HI__))) uint16_mode_t;
typedef int __attribute__((mode(__SI__))) int32_mode_t;
typedef int __attribute__((mode(__DI__))) int64_mode_t;
typedef unsigned int __attribute__((mode(__DI__))) uint64_mode_t;

/* Without underscores: QI, HI, SI, DI */
typedef int __attribute__((mode(QI))) int8_alt_t;
typedef int __attribute__((mode(DI))) int64_alt_t;

/* word mode: pointer-sized */
typedef int __attribute__((mode(__word__))) word_t;

/* Floating-point modes: SF=float, DF=double, TF=long double */
typedef float __attribute__((mode(__SF__))) sf_mode_t;
typedef float __attribute__((mode(__DF__))) df_mode_t;
typedef float __attribute__((mode(__TF__))) tf_mode_t;

/* pointer mode: pointer-sized unsigned integer */
typedef int __attribute__((mode(__pointer__))) pointer_mode_t;

/* Mode attribute after typedef name (less common form) */
typedef int late_mode_t __attribute__((mode(__DI__)));

/* Verify sizes: DI should be 64-bit (8 bytes) */
int64_mode_t x;
_Static_assert(sizeof(x) == 8, "DI mode should be 8 bytes");

uint64_mode_t y;
_Static_assert(sizeof(y) == 8, "unsigned DI mode should be 8 bytes");

/* QI should be 8-bit (1 byte) */
int8_mode_t z;
_Static_assert(sizeof(z) == 1, "QI mode should be 1 byte");

/* HI should be 16-bit (2 bytes) */
uint16_mode_t w;
_Static_assert(sizeof(w) == 2, "HI mode should be 2 bytes");

int main(void) {
    x = 0x123456789ABCDEF0LL;
    y = 0xFEDCBA9876543210ULL;
    z = 42;
    w = 1000;
    return 0;
}
