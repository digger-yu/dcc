// SPDX-License-Identifier: MIT
// 扩展操作数形式测试: 随机数/远控制流/TSX/AVX-imm/MMX/AMX
// 编译: dcc -c -o /dev/null asm_extended_forms.c
// 预期: stderr 不包含 "emitted as nop"

// === 1. 随机数: rdrand/rdseed 非寄存器操作数 ===
void test_rdrand_mem(void) {
    // rdrand 内存形式: F3 0F C7 /6
    asm volatile("rdrand (%rsp)");
    asm volatile("rdrand 8(%rax)");
}

void test_rdseed_mem(void) {
    // rdseed 内存形式: F3 0F C7 /7
    asm volatile("rdseed (%rsp)");
    asm volatile("rdseed 16(%rbx)");
}

// === 2. 远控制流: 无后缀 ljmp/lcall + 间接形式 + imm16 ===
void test_far_direct_nosuffix(void) {
    // ljmp/lcall 无后缀 (默认 32 位偏移)
    asm volatile("ljmp $0x08, $0x12345678");
    asm volatile("lcall $0x08, $0x12345678");
}

void test_far_indirect(void) {
    // ljmp/lcall 间接内存形式: FF /5, FF /3
    asm volatile("ljmpl *(%rsp)");
    asm volatile("ljmpl *(%rax)");
    asm volatile("lcalll *(%rsp)");
    // 无后缀间接形式
    asm volatile("ljmp *(%rsp)");
    asm volatile("lcall *(%rax)");
}

void test_far_imm16(void) {
    // retf/lret 带 imm16
    asm volatile("retf $0x10");
    asm volatile("lret $0x20");
    asm volatile("retfw $0x08");
    asm volatile("lretw $0x08");
}

void test_far_sized(void) {
    // 带后缀的远跳转/远调用
    asm volatile("ljmpl $0x12345678, $0x08");
    asm volatile("lcalll $0x12345678, $0x08");
    asm volatile("ljmpw $0x1234, $0x08");
    asm volatile("lcallw $0x1234, $0x08");
}

// === 3. TSX: xbegin rel32 + xabort imm8 + xend ===
void test_tsx_extended(void) {
    // xbegin 带大偏移立即数 (rel32 形式)
    asm volatile("xbegin $0x1000");
    // xbegin 带小偏移 (rel16 形式)
    asm volatile("xbegin $0x10");
    // xabort 带立即数
    asm volatile("xabort $0x03");
    // xend
    asm volatile("xend");
}

// === 4. AVX 带 imm 三操作数: 内存源操作数 ===
void test_aeskeygen_mem(unsigned long long a) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "aeskeygenassist $0xdb, (%rsp), %%xmm1\n"
        "movd %%xmm1, %0"
        : "+r"(a)
        :
        : "xmm0", "xmm1", "memory"
    );
}

void test_sha1rnds4_mem(unsigned long long a) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "sha1rnds4 $0, (%rsp), %%xmm1\n"
        "movd %%xmm1, %0"
        : "+r"(a)
        :
        : "xmm0", "xmm1", "memory"
    );
}

void test_pshufd_mem(unsigned long long a) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "pshufd $0x1B, (%rsp), %%xmm1\n"
        "movd %%xmm1, %0"
        : "+r"(a)
        :
        : "xmm0", "xmm1", "memory"
    );
}

void test_pshufhw_mem(unsigned long long a) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "pshufhw $0x1B, (%rsp), %%xmm1\n"
        "movd %%xmm1, %0"
        : "+r"(a)
        :
        : "xmm0", "xmm1", "memory"
    );
}

void test_pshuflw_mem(unsigned long long a) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "pshuflw $0x1B, (%rsp), %%xmm1\n"
        "movd %%xmm1, %0"
        : "+r"(a)
        :
        : "xmm0", "xmm1", "memory"
    );
}

void test_palignr_mem(unsigned long long a) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "palignr $4, (%rsp), %%xmm1\n"
        "movd %%xmm1, %0"
        : "+r"(a)
        :
        : "xmm0", "xmm1", "memory"
    );
}

void test_pblendw_mem(unsigned long long a) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "pblendw $0xFF, (%rsp), %%xmm1\n"
        "movd %%xmm1, %0"
        : "+r"(a)
        :
        : "xmm0", "xmm1", "memory"
    );
}

void test_insertps_mem(unsigned long long a) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "insertps $0, (%rsp), %%xmm1\n"
        "movd %%xmm1, %0"
        : "+r"(a)
        :
        : "xmm0", "xmm1", "memory"
    );
}

// === 5. SSE4.1 pinsr/pextr/extractps 内存操作数 ===
void test_pinsrb_mem(void) {
    asm volatile("pinsrb $0, (%rsp), %%xmm0" : : : "xmm0", "memory");
}

void test_pinsrd_mem(void) {
    asm volatile("pinsrd $1, (%rsp), %%xmm0" : : : "xmm0", "memory");
}

void test_pextrb_mem(void) {
    asm volatile("pextrb $0, %%xmm0, (%rsp)" : : : "xmm0", "memory");
}

void test_pextrd_mem(void) {
    asm volatile("pextrd $1, %%xmm0, (%rsp)" : : : "xmm0", "memory");
}

void test_extractps_mem(void) {
    asm volatile("extractps $0, %%xmm0, (%rsp)" : : : "xmm0", "memory");
}

// === 6. AVX vpinsr*/vpextr*/vcvtps2ph 内存操作数 ===
void test_vpinsrb_mem(void) {
    asm volatile("vpinsrb $0, (%rsp), %%xmm0, %%xmm1" : : : "xmm0", "xmm1", "memory");
}

void test_vpextrd_mem(void) {
    asm volatile("vpextrd $1, %%xmm0, (%rsp)" : : : "xmm0", "memory");
}

void test_vcvtps2ph_mem(void) {
    asm volatile("vcvtps2ph $0, %%xmm0, (%rsp)" : : : "xmm0", "memory");
}

// === 7. vextractpd XMM 寄存器目标 ===
void test_vextractpd_xmm(void) {
    asm volatile("vextractpd $1, %%xmm0, %%xmm1" : : : "xmm0", "xmm1");
}

// === 8. MMX: pshufw 寄存器 + 内存源 ===
void test_pshufw_reg(void) {
    asm volatile("pshufw $0x1B, %mm0, %mm1");
    asm volatile("pshufw $0, %mm2, %mm3");
}

void test_pshufw_mem(void) {
    asm volatile("pshufw $0x1B, (%rsp), %mm0");
    asm volatile("pshufw $0, 8(%rax), %mm1");
}

// === 9. AMX: tilezero tmm 寄存器 ===
void test_tilezero(void) {
    asm volatile("tilezero %tmm0");
    asm volatile("tilezero %tmm1");
}

// === 10. vextractps 内存目标 ===
void test_vextractps_mem(unsigned long long a) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "vextractps $0, %%xmm0, (%rsp)\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        :
        : "xmm0", "memory"
    );
}

int main(void) {
    test_rdrand_mem();
    test_rdseed_mem();
    test_far_direct_nosuffix();
    test_far_indirect();
    test_far_imm16();
    test_far_sized();
    test_tsx_extended();
    unsigned long long v = 0;
    test_aeskeygen_mem(v);
    test_sha1rnds4_mem(v);
    test_pshufd_mem(v);
    test_pshufhw_mem(v);
    test_pshuflw_mem(v);
    test_palignr_mem(v);
    test_pblendw_mem(v);
    test_insertps_mem(v);
    test_pinsrb_mem();
    test_pinsrd_mem();
    test_pextrb_mem();
    test_pextrd_mem();
    test_extractps_mem();
    test_vpinsrb_mem();
    test_vpextrd_mem();
    test_vcvtps2ph_mem();
    test_vextractpd_xmm();
    test_pshufw_reg();
    test_pshufw_mem();
    test_tilezero();
    test_vextractps_mem(v);
    return 0;
}
