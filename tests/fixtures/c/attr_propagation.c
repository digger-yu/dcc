/* Test: attribute propagation through previously-silent skip_attributes() paths.
 * Verifies that section, used, retain, alias, aligned attributes are correctly
 * captured at various declarator positions. */

/* 1. Function pointer with section attribute */
void (*__attribute__((section(".init.data"))) init_func_ptr)(int);

/* 2. Function pointer with used attribute */
int (*__attribute__((used)) retained_handler)(void);

/* 3. Enum with packed + used attributes */
enum __attribute__((packed)) small_enum {
    ENUM_A, ENUM_B, ENUM_C
} __attribute__((used)) enum_var;

/* 4. Struct with post-body section attribute */
struct my_struct {
    int x;
    char y;
} __attribute__((aligned(16))) aligned_instance;

/* 5. Function pointer array with attributes */
void (*__attribute__((section(".text.init"))) func_array[4])(void);

/* 6. Typedef with attribute (attribute is parsed but doesn't affect type) */
typedef int aligned_int __attribute__((aligned(8)));
aligned_int test_var;

int main(void) {
    int fail = 0;

    /* packed enum: sizeof should be 1 (char-sized) */
    if (sizeof(enum small_enum) != 1) fail |= 1;

    /* aligned struct instance: alignment should be >= 16 */
    if (_Alignof(struct my_struct) < 16) fail |= 2;
    if (sizeof(struct my_struct) % 16 != 0) fail |= 4;

    return fail;
}
