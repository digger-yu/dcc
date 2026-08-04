// Phase 2: 操作数模式补全测试
// 编译: dcc -c -o /dev/null asm_operand_patterns.c
// 预期: stderr 不包含 "emitted as nop"

int g_data;

// 1.5 ALU unsigned imm8: addb $0xFF, %al
void test_alu_imm8_unsigned(void) {
    asm volatile("addb $0xFF, %al");
    asm volatile("addb $0x80, %al");
    asm volatile("addb $0x00, %al");
    asm volatile("subb $0xFF, %al");
    asm volatile("andb $0xFF, %al");
    asm volatile("orb $0xFF, %al");
    asm volatile("xorb $0xFF, %al");
    asm volatile("cmpb $0xFF, %al");
}

// 1.9 imul2/imul3 SymDisp
void test_imul_symdisp(void) {
    asm volatile("imul %rax, g_data(%rip)");
    asm volatile("imul g_data(%rip), %rax");
    asm volatile("imul $3, g_data(%rip), %rax");
}

// 1.12 far jumps
void test_far_jumps(void) {
    asm volatile("retf $8");
    asm volatile("lretq");
    asm volatile("ljmpl $0x08, $0x12345678");
    asm volatile("lcalll $0x08, $0x12345678");
}

// 1.14 movzx/movsx SymDisp
void test_movzx_symdisp(void) {
    asm volatile("movzbl g_data(%rip), %eax");
    asm volatile("movzwl g_data(%rip), %eax");
    asm volatile("movsbl g_data(%rip), %eax");
    asm volatile("movswl g_data(%rip), %eax");
    asm volatile("movsbl g_data(%rip), %eax");
}

// 1.18 SSE SegMem
void test_sse_segmem(void) {
    asm volatile("movdqa (%rsp), %xmm0");
    asm volatile("addps (%rsp), %xmm0");
    asm volatile("subps (%rsp), %xmm0");
    asm volatile("mulps (%rsp), %xmm0");
    asm volatile("addsd (%rsp), %xmm0");
    asm volatile("subsd (%rsp), %xmm0");
    asm volatile("mulsd (%rsp), %xmm0");
    asm volatile("divsd (%rsp), %xmm0");
    asm volatile("addss (%rsp), %xmm0");
    asm volatile("subss (%rsp), %xmm0");
    asm volatile("mulss (%rsp), %xmm0");
    asm volatile("divss (%rsp), %xmm0");
    asm volatile("xorps (%rsp), %xmm0");
    asm volatile("xorpd (%rsp), %xmm0");
    asm volatile("ucomiss (%rsp), %xmm0");
    asm volatile("ucomisd (%rsp), %xmm0");
}

// 1.19 shld/shrd Mem
void test_shld_mem(void) {
    asm volatile("shldq $3, %rax, (%rsp)");
    asm volatile("shrdq $3, %rax, (%rsp)");
    asm volatile("shldq %cl, %rax, (%rsp)");
    asm volatile("shrdq %cl, %rax, (%rsp)");
}

// 1.20 bt* SegMem
void test_bt_segmem(void) {
    asm volatile("btsq $5, (%rsp)");
    asm volatile("btrq $5, (%rsp)");
    asm volatile("btcq $5, (%rsp)");
    asm volatile("btq $5, (%rsp)");
}

// 1.21 sys_mem SymDisp
void test_sys_mem_symdisp(void) {
    // vmptrld 需要实际内存操作数，用 (%rsp) 代替 sym(%rip) 避免重定位问题
    // 但 SymDisp 路径已在代码中添加，此处测试 Mem 路径
    asm volatile("vmptrld (%rsp)");
}

// 1.23 FMA
void test_fma_segmem(void) {
    asm volatile("vfmadd231sd (%rsp), %xmm0, %xmm1");
    asm volatile("vfmadd231ss (%rsp), %xmm0, %xmm1");
    asm volatile("vfmadd231pd (%rsp), %xmm0, %xmm1");
    asm volatile("vfmadd231ps (%rsp), %xmm0, %xmm1");
}

// 1.24 VEX SegMem
void test_vex_segmem(void) {
    asm volatile("vmovdqu (%rsp), %xmm0");
    asm volatile("vmovdqa (%rsp), %xmm0");
    asm volatile("vaddps (%rsp), %xmm0, %xmm1");
    asm volatile("vaddpd (%rsp), %xmm0, %xmm1");
    asm volatile("vsubps (%rsp), %xmm0, %xmm1");
    asm volatile("vsubpd (%rsp), %xmm0, %xmm1");
}

// 1.26 AES
void test_aes_imm8(void) {
    asm volatile("aeskeygenassist $0x10, %xmm0, %xmm1");
}

// 1.27 xbegin/xabort
void test_tsx(void) {
    asm volatile("xabort $0xFF");
}

// 1.6 test $imm, mem
void test_test_imm_mem(void) {
    asm volatile("testl $0xFF, (%rsp)");
    asm volatile("testq $0xFF, (%rsp)");
}

// 1.7 test $imm, %al
void test_test_imm_al(void) {
    asm volatile("testb $0x80, %al");
    asm volatile("testb $0xFF, %al");
}

// 1.8 shl/shr/sar SegMem
void test_shift_segmem(void) {
    asm volatile("shlq $3, (%rsp)");
    asm volatile("shrq $3, (%rsp)");
    asm volatile("sarq $3, (%rsp)");
    asm volatile("shlq %cl, (%rsp)");
    asm volatile("shrq %cl, (%rsp)");
    asm volatile("sarq %cl, (%rsp)");
}

// 1.10 cmpxchg16b
void test_cmpxchg16b(void) {
    asm volatile("cmpxchg16b (%rsp)");
}

// 1.11 xadd mem, reg
void test_xadd_mem(void) {
    asm volatile("xaddq %rax, (%rsp)");
}

// 1.13 setcc mem
void test_setcc_mem(void) {
    asm volatile("sete (%rsp)");
    asm volatile("setne (%rsp)");
}

// 1.15 popcnt mem
void test_popcnt_mem(void) {
    asm volatile("popcntq (%rsp), %rax");
}

// 1.16 movnti mem
void test_movnti_mem(void) {
    asm volatile("movnti %rax, (%rsp)");
}

// 1.17 prefetch
void test_prefetch_mem(void) {
    asm volatile("prefetcht0 (%rsp)");
    asm volatile("prefetcht1 (%rsp)");
    asm volatile("prefetcht2 (%rsp)");
    asm volatile("prefetchnta (%rsp)");
}

// 1.22 int imm8
void test_int_imm8(void) {
    asm volatile("int $0x80");
}

// 2.2 call *(mem)
void test_call_indirect_mem(void) {
    // 间接调用: call *(%rsp) 和 call *(%rax)
    asm volatile("call *(%rsp)");
}
