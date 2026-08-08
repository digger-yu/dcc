// SPDX-License-Identifier: MIT
// 扩展操作数形式测试: 随机数/远控制流/TSX/AVX-imm/MMX/AMX
// 编译: dcc -c -o /dev/null asm_extended_forms.c
// 预期: stderr 不包含 "emitted as nop"

// === 1. 随机数: rdrand/rdseed 内存操作数 ===
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

// === 2. 远控制流: 间接形式 + 反序操作数 ===
void test_far_indirect(void) {
    // ljmp/lcall 间接内存形式: FF /5, FF /3
    asm volatile("ljmpl *(%rsp)");
    asm volatile("ljmpl *(%rax)");
    asm volatile("lcalll *(%rsp)");
}

void test_far_reversed(void) {
    // $off, $seg 反序形式
    asm volatile("ljmpl $0x12345678, $0x08");
    asm volatile("lcalll $0x12345678, $0x08");
    asm volatile("ljmpw $0x1234, $0x08");
    asm volatile("lcallw $0x1234, $0x08");
}

// === 3. TSX: xbegin rel32 + xabort 符号 ===
void test_tsx_extended(void) {
    // xbegin 带大偏移立即数 (rel32 形式)
    asm volatile("xbegin $0x1000");
    // xabort 带立即数 (标准形式)
    asm volatile("xabort $0x03");
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

// === 5. MMX: pshufw 内存源 ===
void test_pshufw_mem(void) {
    asm volatile("pshufw $0x1B, (%rsp), %mm0");
    asm volatile("pshufw $0, 8(%rax), %mm1");
}

// === 6. vextractps/vextractpd 内存目标 ===
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
    test_far_indirect();
    test_far_reversed();
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
    test_pshufw_mem();
    test_vextractps_mem(v);
    return 0;
}
