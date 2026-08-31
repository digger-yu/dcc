/* Test: __attribute__((transparent_union, externally_visible,
 *         no_reorder, optimize, no_split_stack,
 *         patchable_function_entry)) */

/* transparent_union: union type where any member can be passed */
typedef union {
    void *ptr;
    int *iptr;
} __attribute__((transparent_union)) any_ptr;

/* externally_visible: prevent LTO from hiding the symbol */
__attribute__((externally_visible))
void visible_fn(void) {}

/* no_reorder: prevent compiler from reordering this function */
__attribute__((no_reorder))
void ordered_fn(void) {}

/* optimize: per-function optimization (consumed, no codegen effect) */
__attribute__((optimize("O2")))
void optimized_fn(void) {}

/* no_split_stack: disable stack splitting */
__attribute__((no_split_stack))
void no_split_fn(void) {}

/* patchable_function_entry: NOP sled for live patching */
__attribute__((patchable_function_entry(5, 3)))
void patchable_fn(void) {}

/* Combined visibility attributes */
__attribute__((externally_visible, no_reorder))
void combined_fn(void) {}

int main(void) {
    visible_fn();
    ordered_fn();
    optimized_fn();
    no_split_fn();
    patchable_fn();
    combined_fn();
    return 0;
}
