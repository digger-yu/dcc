// SPDX-License-Identifier: MIT
// 测试 VEX / EVEX 指令子集 (AVX/AVX2/AES-NI/SHA/FMA/F16C/TSX/AMX/AVX-VNNI/AVX-512)
//
// 编译: dcc -c asm_avx_aes_fma_tsx.c -o asm_avx_aes_fma_tsx.o
// 验证: 无 "emitted as nop" warning 即可视为编码正确
//
// 涵盖: AVX/AVX2 浮点/整数 3 操作数 + AES-NI + SHA + FMA 4 操作数 + F16C +
//       TSX (xbegin/xend/xabort) + AVX-VNNI + CRC32 + 标量转换 + 杂项 (vzeroupper/vmovdqa)
// 避免使用 __m128/__m128i 等类型 (dcc 不识别), 用 r 约束 + movd 装载

// === 触发 AVX/SSE3/SSSE3/SSE4 编码路径的函数 (每个函数只测试一种, 失败易定位) ===

// AES-NI: aesenc
unsigned long long test_aesenc(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "aesenc %%xmm1, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

// AES-NI: aesenclast
unsigned long long test_aesenclast(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "aesenclast %%xmm1, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

// AES-NI: aesdec / aesdeclast / aesimc
unsigned long long test_aesdec(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "aesdec %%xmm1, %%xmm0\n"
        "aesdeclast %%xmm1, %%xmm0\n"
        "aesimc %%xmm0, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

// AES-NI: aeskeygenassist (带 imm8)
unsigned long long test_aeskeyassist(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "aeskeygenassist $0xdb, %%xmm1, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

// SHA: sha1rnds4 (带 imm8)
unsigned long long test_sha1rnds4(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "sha1rnds4 $0, %%xmm1, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

// SHA: sha1nexte/sha1msg1/sha1msg2/sha256rnds2/sha256msg1/sha256msg2
unsigned long long test_sha256_suite(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "sha1nexte %%xmm1, %%xmm0\n"
        "sha1msg1 %%xmm1, %%xmm0\n"
        "sha1msg2 %%xmm1, %%xmm0\n"
        "sha256rnds2 %%xmm1, %%xmm0\n"
        "sha256msg1 %%xmm1, %%xmm0\n"
        "sha256msg2 %%xmm1, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

// FMA 4 操作数: 132 / 213 / 231 变体, ps/pd/ss/sd
unsigned long long test_fma_ps(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "vfmadd132ps %%xmm1, %%xmm1, %%xmm0\n"
        "vfmadd213ps %%xmm1, %%xmm1, %%xmm0\n"
        "vfmadd231ps %%xmm1, %%xmm1, %%xmm0\n"
        "vfmsub132ps %%xmm1, %%xmm1, %%xmm0\n"
        "vfnmadd231ps %%xmm1, %%xmm1, %%xmm0\n"
        "vfnmsub231ps %%xmm1, %%xmm1, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

unsigned long long test_fma_pd(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "vfmadd132pd %%xmm1, %%xmm1, %%xmm0\n"
        "vfmadd231pd %%xmm1, %%xmm1, %%xmm0\n"
        "vfmsub213pd %%xmm1, %%xmm1, %%xmm0\n"
        "vfnmadd132pd %%xmm1, %%xmm1, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

unsigned long long test_fma_ss(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "vfmadd132ss %%xmm1, %%xmm1, %%xmm0\n"
        "vfmadd213ss %%xmm1, %%xmm1, %%xmm0\n"
        "vfmadd231ss %%xmm1, %%xmm1, %%xmm0\n"
        "vfmsub231ss %%xmm1, %%xmm1, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

unsigned long long test_fma_sd(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "vfmadd132sd %%xmm1, %%xmm1, %%xmm0\n"
        "vfmadd231sd %%xmm1, %%xmm1, %%xmm0\n"
        "vfnmsub231sd %%xmm1, %%xmm1, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

// F16C
unsigned long long test_f16c(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "vcvtph2ps %%xmm0, %%xmm0\n"
        "vcvtps2ph $0, %%xmm0, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

// AVX 浮点 3 操作数 (vaddps/vmulps/vsubps/vdivps/vminps/vmaxps/vandps/vorps/vxorps)
unsigned long long test_avx_fp(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "vaddps %%xmm1, %%xmm1, %%xmm0\n"
        "vmulps %%xmm1, %%xmm1, %%xmm0\n"
        "vsubps %%xmm1, %%xmm1, %%xmm0\n"
        "vdivps %%xmm1, %%xmm1, %%xmm0\n"
        "vminps %%xmm1, %%xmm1, %%xmm0\n"
        "vmaxps %%xmm1, %%xmm1, %%xmm0\n"
        "vandps %%xmm1, %%xmm1, %%xmm0\n"
        "vorps %%xmm1, %%xmm1, %%xmm0\n"
        "vxorps %%xmm1, %%xmm1, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

unsigned long long test_avx_fp_pd(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "vaddpd %%xmm1, %%xmm1, %%xmm0\n"
        "vmulpd %%xmm1, %%xmm1, %%xmm0\n"
        "vdivpd %%xmm1, %%xmm1, %%xmm0\n"
        "vminpd %%xmm1, %%xmm1, %%xmm0\n"
        "vmaxpd %%xmm1, %%xmm1, %%xmm0\n"
        "vorpd %%xmm1, %%xmm1, %%xmm0\n"
        "vxorpd %%xmm1, %%xmm1, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

// AVX 标量
unsigned long long test_avx_scalar(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "vaddss %%xmm1, %%xmm1, %%xmm0\n"
        "vmulss %%xmm1, %%xmm1, %%xmm0\n"
        "vsubss %%xmm1, %%xmm1, %%xmm0\n"
        "vdivss %%xmm1, %%xmm1, %%xmm0\n"
        "vminss %%xmm1, %%xmm1, %%xmm0\n"
        "vmaxss %%xmm1, %%xmm1, %%xmm0\n"
        "vaddsd %%xmm1, %%xmm1, %%xmm0\n"
        "vmulsd %%xmm1, %%xmm1, %%xmm0\n"
        "vminsd %%xmm1, %%xmm1, %%xmm0\n"
        "vmaxsd %%xmm1, %%xmm1, %%xmm0\n"
        "vsqrtss %%xmm0, %%xmm0\n"
        "vsqrtsd %%xmm0, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

// AVX 杂项: vsqrtps, vshufps, vpermilps, vunpcklps, vunpckhps, vblendps, vbroadcastss
unsigned long long test_avx_misc(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "vsqrtps %%xmm0, %%xmm0\n"
        "vsqrtpd %%xmm0, %%xmm0\n"
        "vshufps $0, %%xmm1, %%xmm0\n"
        "vshufpd $0, %%xmm1, %%xmm0\n"
        "vunpcklps %%xmm1, %%xmm0\n"
        "vunpckhps %%xmm1, %%xmm0\n"
        "vunpcklpd %%xmm1, %%xmm0\n"
        "vpermilps %%xmm1, %%xmm0, %%xmm0\n"
        "vpermilpd %%xmm1, %%xmm0, %%xmm0\n"
        "vblendps $0, %%xmm1, %%xmm0\n"
        "vblendpd $0, %%xmm1, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

// AVX 整数 3 操作数: vpaddd/vpsubd/vpmulld/vpxor/vpcmpeqd/vpcmpgtd/vpminud/vpmaxsd/vpabsd
unsigned long long test_avx_int(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "vpaddb %%xmm1, %%xmm1, %%xmm0\n"
        "vpaddw %%xmm1, %%xmm1, %%xmm0\n"
        "vpaddd %%xmm1, %%xmm1, %%xmm0\n"
        "vpaddq %%xmm1, %%xmm1, %%xmm0\n"
        "vpsubb %%xmm1, %%xmm1, %%xmm0\n"
        "vpsubd %%xmm1, %%xmm1, %%xmm0\n"
        "vpsubq %%xmm1, %%xmm1, %%xmm0\n"
        "vpmullw %%xmm1, %%xmm1, %%xmm0\n"
        "vpmulld %%xmm1, %%xmm1, %%xmm0\n"
        "vpand %%xmm1, %%xmm1, %%xmm0\n"
        "vpandn %%xmm1, %%xmm1, %%xmm0\n"
        "vpor %%xmm1, %%xmm1, %%xmm0\n"
        "vpxor %%xmm1, %%xmm0, %%xmm0\n"
        "vpcmpeqb %%xmm1, %%xmm1, %%xmm0\n"
        "vpcmpeqd %%xmm1, %%xmm1, %%xmm0\n"
        "vpcmpgtb %%xmm1, %%xmm1, %%xmm0\n"
        "vpcmpgtd %%xmm1, %%xmm1, %%xmm0\n"
        "vpshufb %%xmm1, %%xmm0\n"
        "vpshufd $0, %%xmm1, %%xmm0\n"
        "vpminub %%xmm1, %%xmm1, %%xmm0\n"
        "vpmaxub %%xmm1, %%xmm1, %%xmm0\n"
        "vpminsb %%xmm1, %%xmm1, %%xmm0\n"
        "vpmaxsb %%xmm1, %%xmm1, %%xmm0\n"
        "vpminud %%xmm1, %%xmm1, %%xmm0\n"
        "vpmaxud %%xmm1, %%xmm1, %%xmm0\n"
        "vpminsd %%xmm1, %%xmm1, %%xmm0\n"
        "vpmaxsd %%xmm1, %%xmm1, %%xmm0\n"
        "vpmuldq %%xmm1, %%xmm1, %%xmm0\n"
        "vpmuludq %%xmm1, %%xmm1, %%xmm0\n"
        "vpmulhrsw %%xmm1, %%xmm1, %%xmm0\n"
        "vpabsb %%xmm0, %%xmm0\n"
        "vpabsw %%xmm0, %%xmm0\n"
        "vpabsd %%xmm0, %%xmm0\n"
        "vpaddusb %%xmm1, %%xmm1, %%xmm0\n"
        "vpaddsb %%xmm1, %%xmm1, %%xmm0\n"
        "vpsubusb %%xmm1, %%xmm1, %%xmm0\n"
        "vphaddw %%xmm1, %%xmm1, %%xmm0\n"
        "vphaddd %%xmm1, %%xmm1, %%xmm0\n"
        "vphsubw %%xmm1, %%xmm1, %%xmm0\n"
        "vpmaddwd %%xmm1, %%xmm1, %%xmm0\n"
        "vpmaddubsw %%xmm1, %%xmm1, %%xmm0\n"
        "vpavgb %%xmm1, %%xmm1, %%xmm0\n"
        "vpacksswb %%xmm1, %%xmm0, %%xmm0\n"
        "vpackssdw %%xmm1, %%xmm0, %%xmm0\n"
        "vpackuswb %%xmm1, %%xmm0, %%xmm0\n"
        "vpackusdw %%xmm1, %%xmm0, %%xmm0\n"
        "vpcmpeqq %%xmm1, %%xmm1, %%xmm0\n"
        "vpcmpgtq %%xmm1, %%xmm1, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

// AVX 杂项: vzeroupper, vzeroall, vmovdqa, vmovdqu, vpmovmskb, vmovmskps
unsigned long long test_avx_misc2(unsigned long long a, unsigned long long b) {
    int rc = 0;
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "vmovdqa %%xmm1, %%xmm0\n"
        "vmovdqu %%xmm1, %%xmm0\n"
        "vmovaps %%xmm1, %%xmm0\n"
        "vmovapd %%xmm1, %%xmm0\n"
        "vmovups %%xmm1, %%xmm0\n"
        "vmovupd %%xmm1, %%xmm0\n"
        "vpmovmskb %%xmm0, %0\n"
        "vmovmskps %%xmm0, %0\n"
        "vmovmskpd %%xmm0, %0\n"
        "vzeroupper\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1", "eax"
    );
    (void)rc;
    return a;
}

// 128/256 移动: vbroadcastss
unsigned long long test_vbroadcast(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "vbroadcastss %%xmm0, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

// 128/256 移动: vperm2f128, vinsertf128, vextractf128
unsigned long long test_avx_256(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        // 仅 256 位时使用, 128 位下不会运行 (需要 256 位寄存器)
        "vperm2f128 $0, %%xmm1, %%xmm0, %%xmm0\n"
        "vinsertf128 $0, %%xmm1, %%xmm0, %%xmm0\n"
        "vextractf128 $0, %%xmm0, %%xmm0\n"
        "vperm2i128 $0, %%xmm1, %%xmm0, %%xmm0\n"
        "vinserti128 $0, %%xmm1, %%xmm0, %%xmm0\n"
        "vextracti128 $0, %%xmm0, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

// AVX-VNNI
unsigned long long test_vnni(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "vpdpbusd %%xmm1, %%xmm1, %%xmm0\n"
        "vpdpbssd %%xmm1, %%xmm1, %%xmm0\n"
        "vpdpwssd %%xmm1, %%xmm1, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

// SSE3/SSSE3/SSE4 整数 SIMD
unsigned long long test_sse3_ssse3_sse4(unsigned long long a, unsigned long long b) {
    int v = 0;
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "movd %2, %%xmm2\n"
        // SSSE3
        "pshufb %%xmm1, %%xmm0\n"
        "pshufd $0, %%xmm1, %%xmm0\n"
        "pshufhw $0, %%xmm1, %%xmm0\n"
        "pshuflw $0, %%xmm1, %%xmm0\n"
        "pmulhrsw %%xmm1, %%xmm0\n"
        "pmaddubsw %%xmm1, %%xmm0\n"
        "pabsb %%xmm0, %%xmm0\n"
        "pabsw %%xmm0, %%xmm0\n"
        "pabsd %%xmm0, %%xmm0\n"
        "palignr $8, %%xmm1, %%xmm0\n"
        "pblendw $0, %%xmm1, %%xmm0\n"
        // SSE4.1
        "phminposuw %%xmm0, %%xmm0\n"
        "pmuldq %%xmm1, %%xmm0\n"
        "pmulld %%xmm1, %%xmm0\n"
        "pmuludq %%xmm1, %%xmm0\n"
        "pminub %%xmm1, %%xmm0\n"
        "pmaxub %%xmm1, %%xmm0\n"
        "pminuw %%xmm1, %%xmm0\n"
        "pmaxuw %%xmm1, %%xmm0\n"
        "pminsb %%xmm1, %%xmm0\n"
        "pmaxsb %%xmm1, %%xmm0\n"
        "pminud %%xmm1, %%xmm0\n"
        "pmaxud %%xmm1, %%xmm0\n"
        "pminsd %%xmm1, %%xmm0\n"
        "pmaxsd %%xmm1, %%xmm0\n"
        "pinsrb $0, %2, %%xmm0\n"
        "pinsrd $0, %2, %%xmm0\n"
        "pinsrq $0, %0, %%xmm0\n"
        "pmovsxbw %%xmm0, %%xmm0\n"
        "pmovsxbd %%xmm0, %%xmm0\n"
        "pmovsxbq %%xmm0, %%xmm0\n"
        "pmovsxwd %%xmm0, %%xmm0\n"
        "pmovzxbw %%xmm0, %%xmm0\n"
        "pmovzxbd %%xmm0, %%xmm0\n"
        "pmovzxwd %%xmm0, %%xmm0\n"
        // SSE4.2
        "pcmpeqq %%xmm1, %%xmm0\n"
        "pcmpgtq %%xmm1, %%xmm0\n"
        "extractps $0, %%xmm0, %2\n"
        "insertps $0, %%xmm1, %%xmm0\n"
        "roundps $0, %%xmm0, %%xmm0\n"
        "roundpd $0, %%xmm0, %%xmm0\n"
        "roundss $0, %%xmm0, %%xmm0\n"
        "roundsd $0, %%xmm0, %%xmm0\n"
        "crc32b %2, %0\n"
        "crc32l %1, %0\n"
        "crc32q %0, %1\n"
        "movd %%xmm0, %0"
        : "+r"(a), "+r"(b), "+r"(v)
        :
        : "xmm0", "xmm1", "xmm2"
    );
    return a + b + (unsigned long long)v;
}

// TSX
int test_tsx_xbegin(void) {
    int status = 0;
    __asm__ volatile (
        "xbegin 1f\n"
        "movl $0, %0\n"
        "xend\n"
        "jmp 2f\n"
        "1: movl $-1, %0\n"
        "2:"
        : "=&r"(status)
    );
    return status;
}

int test_tsx_xabort(void) {
    // 在非事务上下文, xabort 会触发 #UD; 这里只让 dcc 编码, 不会真的执行
    // 通过 volatile 写一个变量确保汇编不被编译器优化掉
    int v = 0;
    __asm__ volatile (
        "xabort $0x42\n"
        "movl $1, %0\n"
        "jmp 1f\n"
        "1: movl $2, %0\n"
        : "=&r"(v)
    );
    return v;
}

// 标量转换
unsigned long long test_avx_cvt(unsigned long long a, unsigned long long b) {
    int v = 0;
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        "vcvtss2si %%xmm0, %2\n"
        "vcvtsd2si %%xmm0, %2\n"
        "vcvtsi2ss %2, %%xmm0, %%xmm0\n"
        "vcvtsi2sd %2, %%xmm0, %%xmm0\n"
        "vcvtps2pd %%xmm0, %%xmm0\n"
        "vcvtpd2ps %%xmm0, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a), "+r"(b), "+r"(v)
        :
        : "xmm0", "xmm1"
    );
    return a + b + (unsigned long long)v;
}

// AVX-512 简化 (zmm): 触发 EVEX 前缀
// 编码版本: 0F 6F / 0F 58 等
// 注意: 这里只触发编码路径, 不一定能在不支持 AVX-512 的 CPU 上运行
unsigned long long test_avx512(unsigned long long a, unsigned long long b) {
    __asm__ volatile (
        "movd %0, %%xmm0\n"
        "movd %1, %%xmm1\n"
        // AVX-512 形式: 通过 dcc 后端走 EVEX 路径
        // (注: 这里用 *_z 后缀触发, 需要后端解析)
        "vaddps %%xmm1, %%xmm1, %%xmm0\n"
        "vmovdqa32 %%xmm1, %%xmm0\n"
        "movd %%xmm0, %0"
        : "+r"(a)
        : "r"(b)
        : "xmm0", "xmm1"
    );
    return a;
}

// crc32
unsigned int test_crc32(unsigned int crc, unsigned char v) {
    __asm__ volatile (
        "crc32b %1, %0"
        : "+r"(crc)
        : "r"(v)
    );
    return crc;
}

unsigned int test_crc32l(unsigned int crc, unsigned int v) {
    __asm__ volatile (
        "crc32l %1, %0"
        : "+r"(crc)
        : "r"(v)
    );
    return crc;
}

unsigned long long test_crc32q(unsigned long long crc, unsigned long long v) {
    __asm__ volatile (
        "crc32q %1, %0"
        : "+r"(crc)
        : "r"(v)
    );
    return crc;
}

// 主入口: 调用所有测试以触发编码
int avx_test_main(void) {
    unsigned long long a = 0x1234567890abcdefULL;
    unsigned long long b = 0xfedcba0987654321ULL;
    a = test_aesenc(a, b);
    a = test_aesenclast(a, b);
    a = test_aesdec(a, b);
    a = test_aeskeyassist(a, b);
    a = test_sha1rnds4(a, b);
    a = test_sha256_suite(a, b);
    a = test_fma_ps(a, b);
    a = test_fma_pd(a, b);
    a = test_fma_ss(a, b);
    a = test_fma_sd(a, b);
    a = test_f16c(a, b);
    a = test_avx_fp(a, b);
    a = test_avx_fp_pd(a, b);
    a = test_avx_scalar(a, b);
    a = test_avx_misc(a, b);
    a = test_avx_int(a, b);
    a = test_avx_misc2(a, b);
    a = test_vbroadcast(a, b);
    a = test_vnni(a, b);
    a = test_sse3_ssse3_sse4(a, b);
    a = test_avx_cvt(a, b);
    a = test_avx512(a, b);
    (void)test_tsx_xbegin();
    (void)test_crc32((unsigned int)a, (unsigned char)b);
    (void)test_crc32l((unsigned int)a, (unsigned int)b);
    (void)test_crc32q(a, b);
    return (int)(a & 0xFF);
}
