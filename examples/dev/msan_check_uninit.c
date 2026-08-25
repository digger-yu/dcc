/* examples/dev/msan_check_uninit.c
 *
 * MSAN (-fsanitize=memory) example:
 *   - Local variable `x` is declared but not initialized.
 *   - Reading `x` triggers __msan_check_load_4 at the deref site.
 *   - __msan_func_entry is called at function prologue to poison
 *     the stack frame shadow.
 *   - __msan_init is called once at TU init.
 *
 * Expected sanitizer callbacks:
 *   __msan_init              (TU-level, first function)
 *   __msan_func_entry        (every function entry)
 *   __msan_check_load_4      (reading uninitialized int)
 */

int check_uninit(void) {
    int x;           /* uninitialized — shadow marks as poison */
    int y = x + 1;   /* load of x triggers __msan_check_load_4 */
    return y;
}

int store_to_ptr(int *p) {
    *p = 42;         /* store triggers __msan_check_store_4 */
    return *p;       /* load triggers __msan_check_load_4 */
}
