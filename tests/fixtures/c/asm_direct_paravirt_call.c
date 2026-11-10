// §5.32：`call *%[opptr]` 的**字节形状**。
//
// 内核 x86 alternatives 对 `ALT_FLAG_DIRECT_CALL` 站点的替换器
// `arch/x86/kernel/alternative.c::alt_replace_call()` 只认 6 字节的
// `ff 15 disp32`（`call *disp32(%rip)`）：
//
//     if (a->instrlen != 6 || instr[0] != 0xFF || instr[1] != 0x15) {
//         pr_err("SMP alternatives: ALT_FLAG_DIRECT_CALL set for unrecognized indirect call\n");
//         BUG();
//     }
//
// 而 paravirt 的调用点（`arch/x86/include/asm/paravirt_types.h` 的
// `paravirt_call` 族）是 `asm volatile("call *%[opptr]" :: [opptr] "m"(pv_ops.cpu.x))`。
// dcc 过去把 `"m"` 操作数的地址物化进一个栈槽，编成：
//
//     lea pv_ops+off(%rip),%rax
//     mov %rax,-N(%r13)
//     mov -N(%r13),%rbx
//     rex call *(%rbx)          # 40 ff 13
//
// 语义等价、字节形状不同 ⇒ boot 1.9 s 在 alternative.c:558 BUG()。
//
// 本夹具只要「编出来的指令形状」，不执行（未初始化的函数指针调用会崩）。

struct pv_cpu_ops {
	void (*irq_disable)(void);
	void (*irq_enable)(void);
	void (*safe_halt)(void);
};

struct paravirt_patch {
	unsigned int irq_disable;
	unsigned int irq_enable;
};

struct pv_ops {
	struct pv_cpu_ops cpu;
	struct paravirt_patch patch;
};

struct pv_ops pv_ops;

// 全局对象的常量字段链：必须发 `call *pv_ops+off(%rip)`（ff 15 disp32）。
void pv_call_irq_disable(void)
{
	asm volatile("call *%[opptr]" :: [opptr] "m" (pv_ops.cpu.irq_disable) : "memory", "cc");
}

// 字段偏移非零的第二条（`cpu.safe_halt` 在 offset 16）。
void pv_call_safe_halt(void)
{
	asm volatile("call *%[opptr]" :: [opptr] "m" (pv_ops.cpu.safe_halt) : "memory", "cc");
}

// 反向对照：基址是**指针参数**，地址是一次加载值，只能走寄存器形式
// `call *(%r..)`。这一半防「把动态地址也折成 rip 相对」的过修。
void pv_call_via_ptr(struct pv_cpu_ops *p)
{
	asm volatile("call *%[opptr]" :: [opptr] "m" (p->irq_enable) : "memory", "cc");
}
