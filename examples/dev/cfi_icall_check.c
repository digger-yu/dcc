/* examples/dev/cfi_icall_check.c
 *
 * CFI (-fsanitize=cfi-icall) example:
 *   - Indirect calls through function pointers are checked
 *     at runtime via __cfi_check(callee_ptr, type_hash).
 *   - Direct calls (callee is a known Ident) are NOT checked.
 *   - type_hash is computed using FNV-1a 64-bit over the
 *     function type representation.
 *
 * Expected sanitizer callbacks:
 *   __cfi_check   (before indirect call through deref)
 *
 * Supported CFI sub-channels:
 *   -fsanitize=cfi           (all sub-channels)
 *   -fsanitize=cfi-icall     (indirect call check)
 *   -fsanitize=cfi-vcall     (virtual call check)
 *   -fsanitize=cfi-nvcall    (non-virtual call check)
 *   -fsanitize=cfi-mfcall    (member function call check)
 *   -fsanitize=cfi-derived-cast
 *   -fsanitize=cfi-unrelated-cast
 */

typedef int (*fn_ptr_t)(int);

int add_one(int x) {
    return x + 1;
}

int call_indirect(fn_ptr_t fn, int arg) {
    /* Direct call — no CFI check */
    int a = add_one(arg);

    /* Indirect call through deref — triggers __cfi_check */
    int b = (*fn)(arg);

    return a + b;
}

int call_direct(void) {
    /* Direct call — no CFI check emitted */
    return add_one(42);
}
