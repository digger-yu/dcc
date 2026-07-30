/* M2.4: pointer arithmetic - void*, function ptrs, multi-level ptrs */
#include <stdio.h>
#include <stddef.h>
#include <stdint.h>

static int fails = 0;

#define CHECK(cond, msg) do { \
    if (!(cond)) { \
        printf("FAIL: %s\n", msg); \
        fails++; \
    } \
} while (0)

typedef int (*binop_t)(int, int);

static int add_fn(int a, int b) { return a + b; }
static int mul_fn(int a, int b) { return a * b; }

int main(void) {
    /* Multi-level pointers */
    int val = 42;
    int *p1 = &val;
    int **p2 = &p1;
    CHECK(**p2 == 42, "double pointer dereference");

    /* Function pointers */
    binop_t op = add_fn;
    CHECK(op(3, 4) == 7, "function pointer call");
    op = mul_fn;
    CHECK(op(3, 4) == 12, "function pointer reassign");

    /* NULL pointer */
    int *np = 0;
    CHECK(np == NULL, "NULL pointer comparison");
    void *vnull = NULL;
    CHECK(vnull == NULL, "void* NULL comparison");
    CHECK(np == vnull, "int* NULL == void* NULL");

    /* sizeof pointer types */
    CHECK(sizeof(int*) == 8, "sizeof(int*)");
    CHECK(sizeof(int**) == 8, "sizeof(int**)");
    CHECK(sizeof(void*) == 8, "sizeof(void*)");
    CHECK(sizeof(binop_t) == 8, "sizeof(func_ptr)");

    /* Pointer to struct member */
    struct { int x; int y; } pt = {10, 20};
    int *px = &pt.x;
    int *py = &pt.y;
    CHECK(*px == 10, "struct member ptr x");
    CHECK(*py == 20, "struct member ptr y");

    /* Array of pointers */
    int a = 1, b = 2, c = 3;
    int *arr[3] = {&a, &b, &c};
    CHECK(*arr[0] == 1, "ptr array [0]");
    CHECK(*arr[1] == 2, "ptr array [1]");
    CHECK(*arr[2] == 3, "ptr array [2]");

    if (fails == 0) {
        printf("ALL POINTER CHECKS PASSED\n");
    } else {
        printf("%d pointer checks FAILED\n", fails);
    }
    return fails;
}
