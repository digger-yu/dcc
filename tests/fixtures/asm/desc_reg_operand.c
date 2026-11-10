/* tests/fixtures/asm/desc_reg_operand.c
 *
 * 描述符表指令（`0F 00 /x`）与 FSGSBASE（`F3 0F AE /x`）的**寄存器形式**：
 * 机器码里 ModRM 的 mod 必须是 11。少了那两位就变成内存操作数，指令照常
 * 编码、长度只差一个前缀字节，objdump 也照常反汇编，只是含义从“用这个
 * 选择子”变成“读这个选择子指向的地址”。
 *
 * 现场（报告 §5.19）：内核 `arch/x86/include/asm/desc.h` 的
 *   static inline void native_load_tr_desc(void)
 *   { ...; asm volatile("ltr %w0"::"q" (GDT_ENTRY_TSS*8)); ... }
 * 在 `-c` 直发对象里被编成 `40 0f 00 18` = `ltr (%rax)`，rax 正好是选择子
 * 0x40 ⇒ PVH 启动在 `cpu_init_exception_handling+0xe50` 读线性地址 0x40，
 * #PF 在早期异常栈上叠成 #DF（`PANIC: early exception 0x08 … cr2 0x40`）。
 * 同一份源码走 `-S` 输出了 `ltr %ax`，所以只有直发对象这条路是错的。
 *
 * 本夹具把两条发射路径（`-c` 与 `-S` → GAS）放在一起比：GAS 的编码是判据，
 * 它对这些指令的前缀习惯也是判据的一部分（`str`/`sldt` 要 0x66，`ltr`/
 * `lldt`/`verr`/`verw` 不要 —— `as` 实测）。`rdgsbase` 那一支同一种错法
 * （`0F AE` 的寄存器形式），一起钉住。
 *
 * 指令在用户态不可执行（`ltr`/`lldt` 要 CPL0，`rdgsbase` 要 CR4.FSGSBASE），
 * 所以判据只能是机器码，不能是运行结果。函数体尽量短，比较时按
 * mnemonic+operands 逐行对照，帧与 nop 填充由归一化去掉。
 */

typedef unsigned short u16;

static u16 saved;

void dcc_ltr(void) {
    __asm__ volatile("ltr %w0" ::"q"((u16)0x40));
}

void dcc_lldt(void) {
    __asm__ volatile("lldt %w0" ::"q"((u16)0x40));
}

void dcc_str(void) {
    u16 t;
    __asm__ volatile("str %w0" : "=r"(t));
    saved = t;
}

void dcc_sldt(void) {
    u16 t;
    __asm__ volatile("sldt %w0" : "=r"(t));
    saved = t;
}

void dcc_verr(void) {
    __asm__ volatile("verr %w0" ::"q"((u16)0x40));
}

void dcc_verw(void) {
    __asm__ volatile("verw %w0" ::"q"((u16)0x40));
}

unsigned long dcc_rdgsbase(void) {
    unsigned long v;
    __asm__ volatile("rdgsbase %0" : "=r"(v));
    return v;
}

void dcc_wrgsbase(unsigned long v) {
    __asm__ volatile("wrgsbase %0" ::"r"(v));
}
