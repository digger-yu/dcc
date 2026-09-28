/* Test: __attribute__((transparent_union, externally_visible,
 *         no_reorder, optimize, no_split_stack,
 *         patchable_function_entry)) */

/* transparent_union: union type where any member can be passed */
typedef union {
    void *ptr;
    int *iptr;
} __attribute__((transparent_union)) any_ptr;

/* transparent_union 的**真实语义**：调用点可以用任一成员类型的实参
 * 代替联合本身，无需显式转换。不写这一段时本夹具只钉住“属性能被解析”，
 * 传参语义完全无人考核。gcc 与该属性共存下接受 `take(&v)`，所以这两行
 * 同时是一个可对照的语义测试。 */
int take(any_ptr p);
int take(any_ptr p) {
    return p.iptr != 0 ? 1 : 0;
}

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
    int v = 0;
    void *np = 0;

    visible_fn();
    ordered_fn();
    optimized_fn();
    no_split_fn();
    patchable_fn();
    combined_fn();

    /* 两个调用点分别用 `void *` 与 `int *` 实参，都必须被隐式视为 any_ptr */
    if (take(np) != 0)
        return 1;
    if (take(&v) != 1)
        return 2;
    return 0;
}
