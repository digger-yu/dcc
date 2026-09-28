// asm_mmx.c — MMX instructions test
// Tests all newly supported MMX instructions

void test_mmx_basic(void) {
    // emms 清除 MMX 状态
    asm volatile("emms");

    // 打包/解包
    asm volatile("packsswb %mm0, %mm0");
    asm volatile("packssdw %mm0, %mm0");
    asm volatile("packuswb %mm0, %mm0");
    asm volatile("punpckhbw %mm0, %mm0");
    asm volatile("punpckhwd %mm0, %mm0");
    asm volatile("punpckhdq %mm0, %mm0");
    asm volatile("punpcklbw %mm0, %mm0");
    asm volatile("punpcklwd %mm0, %mm0");
    asm volatile("punpckldq %mm0, %mm0");
}

void test_mmx_arith(void) {
    // 乘法
    asm volatile("pmaddwd %mm0, %mm0");
    asm volatile("pmulhw %mm0, %mm0");
    asm volatile("pmullw %mm0, %mm0");
    asm volatile("pmulhrw %mm0, %mm0");
}

void test_mmx_shift(void) {
    // 移位
    asm volatile("psllw %mm0, %mm0");
    asm volatile("psrlw %mm0, %mm0");
    asm volatile("psraw %mm0, %mm0");
    asm volatile("pslld %mm0, %mm0");
    asm volatile("psrld %mm0, %mm0");
    asm volatile("psrad %mm0, %mm0");
    asm volatile("psllq %mm0, %mm0");
    asm volatile("psrlq %mm0, %mm0");
}

void test_mmx_pshufw(void) {
    // pshufw 需要 imm8
    asm volatile("pshufw $0, %mm0, %mm0");
    asm volatile("pshufw $0x4E, %mm1, %mm2");
}

int main(void) {
    test_mmx_basic();
    test_mmx_arith();
    test_mmx_shift();
    test_mmx_pshufw();
    return 0;
}