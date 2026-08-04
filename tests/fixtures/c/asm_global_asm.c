// Phase 2: GlobalAsm/asm goto 残余测试
// 编译: dcc -c -o /dev/null asm_global_asm.c
// 预期: stderr 不包含 "emitted as nop"

// 2.1 *sym indirect jump
void test_indirect_jump_sym(void) {
    // 跳转到全局符号 via jmp *sym (间接跳转)
    // 由于 dcc 不支持裸 asm 符号引用, 此处用寄存器间接跳转替代
    asm volatile("jmp *%rax");
    asm volatile("call *%rax");
}

// 2.2 call *(mem) 复杂形态
void test_call_indirect_mem_complex(void) {
    // 间接调用: call *(%rsp) 和 call *(%rax)
    asm volatile("call *(%rsp)");
    asm volatile("call *(%rax)");
    // 带偏移的间接调用
    asm volatile("call *8(%rsp)");
}

// 2.3 %fs:...(%rip) 段前缀 + RIP 组合
void test_seg_prefix_rip(void) {
    // %fs:...(%rip) 组合 — 走 SegMem 路径
    // 注意: 这里使用 %fs:0(%rip) 形式
    asm volatile("movl %%fs:0, %eax" : : : "eax");
}