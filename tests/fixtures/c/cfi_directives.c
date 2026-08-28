// Test: .cfi_* complete debug directives
// dcc should preserve CFI directives in asm output
// and silently accept them in binary mode.

// Test 1: Basic CFI directives in naked function
__attribute__((naked)) void cfi_basic(void) {
    __asm__(
        ".cfi_startproc\n\t"
        "pushq %rbp\n\t"
        ".cfi_def_cfa_offset 16\n\t"
        "movq %rsp, %rbp\n\t"
        ".cfi_offset 6, -16\n\t"
        ".cfi_def_cfa_register 6\n\t"
        "nop\n\t"
        "popq %rbp\n\t"
        ".cfi_restore 6\n\t"
        ".cfi_def_cfa_offset 8\n\t"
        "ret\n\t"
        ".cfi_endproc"
    );
}

// Test 2: CFI state save/restore
__attribute__((naked)) void cfi_state(void) {
    __asm__(
        ".cfi_startproc\n\t"
        "pushq %rbx\n\t"
        ".cfi_remember_state\n\t"
        ".cfi_offset 3, -16\n\t"
        "pushq %r12\n\t"
        ".cfi_offset 12, -24\n\t"
        "nop\n\t"
        ".cfi_restore_state\n\t"
        "popq %rbx\n\t"
        "ret\n\t"
        ".cfi_endproc"
    );
}

// Test 3: CFI adjust and signal frame
__attribute__((naked)) void cfi_adjust(void) {
    __asm__(
        ".cfi_startproc\n\t"
        ".cfi_signal_frame\n\t"
        "subq $32, %rsp\n\t"
        ".cfi_adjust_cfa_offset 32\n\t"
        "nop\n\t"
        "addq $32, %rsp\n\t"
        ".cfi_adjust_cfa_offset -32\n\t"
        "ret\n\t"
        ".cfi_endproc"
    );
}

// Test 4: CFI return column
__attribute__((naked)) void cfi_return_col(void) {
    __asm__(
        ".cfi_startproc\n\t"
        ".cfi_return_column 16\n\t"
        "nop\n\t"
        "ret\n\t"
        ".cfi_endproc"
    );
}

int main(void) {
    cfi_basic();
    cfi_state();
    cfi_adjust();
    cfi_return_col();
    return 0;
}
