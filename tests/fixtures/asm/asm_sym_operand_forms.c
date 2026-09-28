// SPDX-License-Identifier: MIT
// 符号操作数形式测试: 随机数/远控制流/TSX/AVX-imm/MMX 的 Sym/SymDisp 操作数
// 编译: dcc -c -o /dev/null asm_sym_operand_forms.c
// 预期: stderr 不包含 "emitted as nop"

// === 1. 随机数: rdrand/rdseed 符号操作数 (RIP-relative) ===
unsigned short rand_val;
unsigned short seed_val;

void test_rdrand_sym(void) {
    // rdrand 符号形式: RIP-relative 内存目标
    asm volatile("rdrand rand_val(%rip)");
    asm volatile("rdrand rand_val");
}

void test_rdseed_sym(void) {
    // rdseed 符号形式: RIP-relative 内存目标
    asm volatile("rdseed seed_val(%rip)");
    asm volatile("rdseed seed_val");
}

// === 2. 远控制流: retf/lret 符号操作数 ===
void test_retf_sym(void) {
    asm volatile("retf $stack_cleanup");
    asm volatile("lret $stack_cleanup");
}

// === 3. 远跳转: ljmpw/ljmpl 带符号偏移 ===
void test_ljmp_sym(void) {
    asm volatile("ljmpl $0x08, far_target");
    asm volatile("lcalll $0x08, far_target");
    asm volatile("ljmpw $0x08, far_target_w");
    asm volatile("lcallw $0x08, far_target_w");
}

// === 4. TSX: xbegin 符号目标 ===
void test_xbegin_sym(void) {
    asm volatile("xbegin txn_abort_handler");
}

// === 5. AVX 带 imm: 符号源操作数 ===
void test_aeskeygen_sym(unsigned long long a) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "aeskeygenassist $0xdb, aes_const(%rip), %%xmm1\n"
        "movd %%xmm1, %0"
        : "+r"(a)
        :
        : "xmm0", "xmm1", "memory"
    );
}

void test_pshufd_sym(unsigned long long a) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "pshufd $0x1B, shuffle_const(%rip), %%xmm1\n"
        "movd %%xmm1, %0"
        : "+r"(a)
        :
        : "xmm0", "xmm1", "memory"
    );
}

void test_palignr_sym(unsigned long long a) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "palignr $4, align_const(%rip), %%xmm1\n"
        "movd %%xmm1, %0"
        : "+r"(a)
        :
        : "xmm0", "xmm1", "memory"
    );
}

void test_insertps_sym(unsigned long long a) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "insertps $0, insert_const(%rip), %%xmm1\n"
        "movd %%xmm1, %0"
        : "+r"(a)
        :
        : "xmm0", "xmm1", "memory"
    );
}

// === 6. MMX pshufw 符号源 ===
void test_pshufw_sym(void) {
    asm volatile("pshufw $0x1B, mmx_const(%rip), %mm0");
}

// === 7. vextractps/vextractpd 符号目标 ===
void test_vextractps_sym(unsigned long long a) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "vextractps $0, %%xmm0, extract_dst(%rip)\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        :
        : "xmm0", "memory"
    );
}

int main(void) {
    test_rdrand_sym();
    test_rdseed_sym();
    test_retf_sym();
    test_ljmp_sym();
    test_xbegin_sym();
    unsigned long long v = 0;
    test_aeskeygen_sym(v);
    test_pshufd_sym(v);
    test_palignr_sym(v);
    test_insertps_sym(v);
    test_pshufw_sym();
    test_vextractps_sym(v);
    return 0;
}
