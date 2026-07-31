/* M2.1: type system - sizeof/alignof for all basic types */
#include <stdio.h>
#include <stddef.h>
#include <stdint.h>

/* function pointer via typedef (direct sizeof(void(*)(void)) not supported) */
typedef void (*void_func_t)(void);
static int fails = 0;
#define CHECK_SIZEOF(type, expected) do { \
    if (sizeof(type) != (expected)) { \
        printf("FAIL sizeof(%s) = %zu, expected %zu\n", \
               #type, sizeof(type), (size_t)(expected)); \
        fails++; \
    } \
} while (0)

#define CHECK_ALIGNOF(type, expected) do { \
    if (_Alignof(type) != (expected)) { \
        printf("FAIL _Alignof(%s) = %zu, expected %zu\n", \
               #type, _Alignof(type), (size_t)(expected)); \
        fails++; \
    } \
} while (0)

int main(void) {
    /* Fundamental integer types */
    CHECK_SIZEOF(char, 1);
    CHECK_SIZEOF(signed char, 1);
    CHECK_SIZEOF(unsigned char, 1);
    CHECK_SIZEOF(short, 2);
    CHECK_SIZEOF(unsigned short, 2);
    CHECK_SIZEOF(int, 4);
    CHECK_SIZEOF(unsigned int, 4);
    CHECK_SIZEOF(long, 8);
    CHECK_SIZEOF(unsigned long, 8);
    CHECK_SIZEOF(long long, 8);
    CHECK_SIZEOF(unsigned long long, 8);

    /* Floating point */
    CHECK_SIZEOF(float, 4);
    CHECK_SIZEOF(double, 8);
    CHECK_SIZEOF(long double, 16);

    /* Pointer types */
    CHECK_SIZEOF(void*, 8);
    CHECK_SIZEOF(int*, 8);
    CHECK_SIZEOF(char*, 8);
    CHECK_SIZEOF(long*, 8);
    CHECK_SIZEOF(double*, 8);
    CHECK_SIZEOF(void_func_t, 8);

    /* size_t, ptrdiff_t */
    CHECK_SIZEOF(size_t, 8);
    CHECK_SIZEOF(ptrdiff_t, 8);
    CHECK_SIZEOF(intptr_t, 8);
    CHECK_SIZEOF(uintptr_t, 8);

    /* stdint exact-width types */
    CHECK_SIZEOF(int8_t, 1);
    CHECK_SIZEOF(int16_t, 2);
    CHECK_SIZEOF(int32_t, 4);
    CHECK_SIZEOF(int64_t, 8);
    CHECK_SIZEOF(uint8_t, 1);
    CHECK_SIZEOF(uint16_t, 2);
    CHECK_SIZEOF(uint32_t, 4);
    CHECK_SIZEOF(uint64_t, 8);

    /* Boolean */
    CHECK_SIZEOF(_Bool, 1);

    /* Enum */
    CHECK_SIZEOF(enum { A, B, C }, 4);

    /* Alignof checks */
    CHECK_ALIGNOF(char, 1);
    CHECK_ALIGNOF(short, 2);
    CHECK_ALIGNOF(int, 4);
    CHECK_ALIGNOF(long, 8);
    CHECK_ALIGNOF(long long, 8);
    CHECK_ALIGNOF(float, 4);
    CHECK_ALIGNOF(double, 8);
    CHECK_ALIGNOF(long double, 16);
    CHECK_ALIGNOF(void*, 8);
    CHECK_ALIGNOF(size_t, 8);
    CHECK_ALIGNOF(_Bool, 1);
    CHECK_ALIGNOF(int8_t, 1);
    CHECK_ALIGNOF(int16_t, 2);
    CHECK_ALIGNOF(int32_t, 4);
    CHECK_ALIGNOF(int64_t, 8);

    if (fails == 0) {
        printf("ALL TYPE SIZE/ALIGN CHECKS PASSED\n");
    } else {
        printf("%d checks FAILED\n", fails);
    }
    return fails;
}
