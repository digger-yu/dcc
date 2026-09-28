/* Test: GNU asm label symbol naming
 * Verifies that __asm__("label") overrides the ELF symbol name
 * for both functions and variables.
 *
 * This is critical for Linux kernel builds where asm labels
 * control exported symbol names for linking.
 */

/* Variable with asm label - ELF symbol should be "real_data_var" not "data_alias" */
int data_alias __asm__("real_data_var") = 42;

/* Uninitialized variable with asm label */
int bss_alias __asm__("real_bss_var");

/* Function declaration with asm label */
void func_alias(void) __asm__("real_func_name");
void defined_func_alias(void) __asm__("my_defined_func");

/* Function definition with matching ELF name */
void defined_func_alias(void) {
    bss_alias = data_alias;
}

/* Use the asm-labeled entities */
int main(void) {
    data_alias = 100;
    defined_func_alias();
    if (bss_alias != 100) return 1;
    return 0;
}

/* Function definition matching the declaration with asm label */
void real_func_name(void) {
    data_alias++;
}
