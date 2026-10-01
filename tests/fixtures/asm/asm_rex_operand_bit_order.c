/* x86_64 的 REX 位序（REX.R→ModRM.reg、REX.B→ModRM.r/m 与 opcode 低 3 位）与
 * 「%spl/%bpl/%sil/%dil 只要有任意 REX 前缀就合法」这两条，一旦被混为一谈，产物就会
 * 静默写坏**别的**寄存器的位段。修前实测（同一份代码，objdump 的权威释义）：
 *
 *   movb $1,"=S"  → b6 01      mov $0x1,%dh      值落进 %rsi 的 8..15 位，内存从未被写
 *   addb $16,"+D" → 80 c7 10   add $0x10,%bh     同上（改的是 %rdi 的高位段）
 *   sete "=S"     → 0f 94 c6   sete %dh          同上
 *   orb $1,"+S"   → 88 ...     mov %cl,(%rsi)    `0x80 + op*8` 把 opcode 撞成了别的指令
 *   shlq $2,"+S"(char) → C1   16/32/64 位的移位 opcode 被拿来搬 8 位
 *   "=m" 的 char 输出 → 41 89 1c 24  mov %ebx,(%r12)   该写 1 字节却写了 4 字节（越界）
 *
 * 「以任意 GP 作移位计数」不在本夹具里：AT&T 只允许 imm16/%cl，gcc 直接拒绝这种写法
 * （`inconsistent operand constraints`），所以它由编码层门禁
 * `codegen::x86_64::rex_matrix::emit_asm_shift_reg_count_matrix` 逐寄存器对钉住。
 *
 * 判据必须是「运行结果与 gcc 逐档一致」，不能是「dcc 自己不报错」：这些编码全部
 * 编得出来、也跑得起来，只是写到了别人的位段上。
 */
#include <stdio.h>

static char cb;
static long lb;

/* `B0+rd ib`（mov $imm8, r/m8）：寄存器号在 opcode 低 3 位，扩展它的是 REX.B；
 * 而 sil/dil（字段值 6/7）只需要「存在 REX」。两者不能混：为 4..7 置 B 会把目标
 * 搬到 r12b..r15b。这里用 "=S"/"=D" 把字段钉死，所以编码是确定的。 */
__attribute__((noinline)) static void mov_imm_to_low_byte(void) {
    char a = 0, d = 0;
    __asm__ volatile("movb $65, %0" : "=S"(a));   /* 期望 movb $65,%sil */
    __asm__ volatile("movb $66, %0" : "=D"(d));   /* 期望 movb $66,%dil */
    cb = a + d;                                   /* 65+66=131，但 `char` 在 x86 是带符号的
                                                   * 所以存回来是 -125（与 gcc 同一口径）*/
}

/* `80 /digit ib`：ModRM.reg 是 opcode 扩展(/0..7)，绝不可被 REX.R 扩成 /8..15
 * （那是未定义操作码）。八个运算的 opcode 都是 0x80，只靠 /digit 区分。 */
__attribute__((noinline)) static void alu_imm8_family(void) {
    char v = 0;
    __asm__ volatile("addb $16, %0" : "+D"(v));   /* v = 16  */
    __asm__ volatile("orb $1, %0" : "+S"(v));     /* v = 17  */
    __asm__ volatile("andb $0x1f, %0" : "+S"(v)); /* v = 17  */
    __asm__ volatile("subb $2, %0" : "+S"(v));    /* v = 15  */
    __asm__ volatile("xorb $4, %0" : "+S"(v));    /* v = 11  */
    __asm__ volatile("cmpb $11, %0" : "+S"(v));   /* v 不变  */
    cb = v;
}

/* `0F 90+cc /0`（setcc r/m8）：4..7 不是 %ah/%ch/%dh/%bh —— 本仓库的寄存器名表把
 * 那四个折叠到了 0..3，所以 8 位上下文里的 4..7 只可能是 %spl..%dil。 */
__attribute__((noinline)) static void setcc_low_byte(void) {
    char one = 1, zero = 0;
    __asm__ volatile("cmpb %1, %1\n\tsete %0" : "=S"(one) : "r"(one));
    __asm__ volatile("cmpb $0, %1\n\tsete %0" : "=D"(zero) : "r"(one));
    cb = one + zero;                              /* 1 + 0 */
}

/* 移位：8 位用 `C0/D0`，16/32/64 才用 `C1/D1`；以任意 GP 作计数时先要
 * `mov 计数, %cl`，而那条 mov 的计数在 ModRM.reg —— 需要 REX.R，不是 REX.B。 */
__attribute__((noinline)) static void shift_forms(void) {
    char b = 3;
    long q = 5;
    __asm__ volatile("shlb $2, %0" : "+S"(b));    /* 8 位: C0 /4 ib  -> 12 */
    __asm__ volatile("shlq $1, %0" : "+D"(q));    /* 64 位: C1 /4 ib -> 10 */
    __asm__ volatile("shlq %%cl, %0" : "+D"(q) : "c"(2L));  /* 计数走 %cl: D3 /4 -> 40 */
    lb = q;
    cb = b;
}

/* `"=m"` 的 char 输出：存回必须走 `88 /r`，写成 `89 /r` 会越界盖掉相邻的 3 字节。 */
__attribute__((noinline)) static void byte_output_to_memory(void) {
    char buf[8];
    long guard = -1;
    for (int i = 0; i < 8; i++) buf[i] = 0;
    char out = 0;
    __asm__ volatile("movb $77, %0" : "=m"(out));
    __asm__ volatile("" : : : "memory");
    buf[0] = out;
    lb = guard;                                   /* 守卫必须仍是 -1 */
    cb = buf[0];
}

int main(void) {
    mov_imm_to_low_byte();
    long a = cb;                                  /* -125（= 131 按 signed char 回绕）*/
    alu_imm8_family();
    long b = cb;                                  /* 11  */
    setcc_low_byte();
    long c = cb;                                  /* 1   */
    byte_output_to_memory();
    long d = cb, dguard = lb;                     /* 77, -1 */
    shift_forms();
    long e = cb, f = lb;                          /* 12, 40 */
    printf("a=%ld b=%ld c=%ld d=%ld dguard=%ld e=%ld f=%ld\n", a, b, c, d, dguard, e, f);
    return (a == -125 && b == 11 && c == 1 && d == 77 && dguard == -1 && e == 12 && f == 40) ? 0 : 1;
}
