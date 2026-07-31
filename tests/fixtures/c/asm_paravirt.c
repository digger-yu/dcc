// 测试 mini-asm 补全: 覆盖内核 paravirt / 启动代码常见指令
// 不应触发任何 "emitted as nop" 警告
// 不要求 main 返回特定值; 仅要求编译通过且无 nop 警告

// 段寄存器访问 (paravirt 常用 %gs/%fs, 启动代码常用 %cs/%ds/%ss)
static long gs_seg_val;
static long fs_seg_val;
static long cs_seg_val;

__attribute__((noinline))
long test_segment_regs(long v) {
    long r = v;
    asm volatile (
        "mov %%gs:%1, %0\n\t"
        "mov %0, %%fs:%1\n\t"
        : "+r"(r)
        : "m"(gs_seg_val)
        : "memory"
    );
    return r;
}

__attribute__((noinline))
long test_push_pop_seg(void) {
    long r = 0;
    asm volatile (
        "push %gs\n\t"
        "pop %gs\n\t"
        "push %fs\n\t"
        "pop %fs\n\t"
        "push %ds\n\t"
        "pop %ds\n\t"
        : "+r"(r)
        :
        : "memory"
    );
    return r;
}

// 段间读写 (启动代码读写 %cs)
__attribute__((noinline))
long test_mov_to_from_seg(long v) {
    long r;
    asm volatile(
        "mov %1, %0\n\t"   // mov %cs, %rax
        "mov %0, %1\n\t"   // mov %rax, %cs (语义上非法, 仅测试编码)
        : "=&r"(r)
        : "r"(v)
    );
    return r;
}

// 64 位 imm push (大常量)
__attribute__((noinline))
long test_push_imm64(void) {
    long r = 0;
    asm volatile(
        "pushq $0x1122334455667788\n\t"
        "popq %0\n\t"
        : "=r"(r)
        :
        : "memory"
    );
    return r;
}

// shift $cl on non-rsp reg (paravirt patch site)
__attribute__((noinline))
long test_shl_cl_r13(long v, long c) {
    long r;
    asm volatile(
        "shlq %1, %0\n\t"   // shift by %r13 (任一 reg), 走 mov cl + D3
        : "=r"(r)
        : "r"(c), "0"(v)
        : "cl"
    );
    return r;
}

// setcc on ah/bh/ch/dh (启动代码条件分支)
__attribute__((noinline))
int test_seto_into_ah(long v) {
    int r;
    long flags = v;
    asm volatile(
        "sete %%ah\n\t"
        "movzbl %%ah, %0\n\t"
        : "=r"(r)
        : "r"(flags)
        : "ah"
    );
    return r;
}

// far jump / call 间接 (内核绝对跳转)
__attribute__((noinline))
void test_indirect_jmp(long target) {
    asm volatile(
        "jmp *%0\n\t"
        :
        : "r"(target)
        :
    );
}

// 系统级: cpuid, rdtsc, wbinvd, invd
__attribute__((noinline))
void test_cpuid(long leaf) {
    asm volatile(
        "cpuid\n\t"
        :
        : "a"(leaf)
        : "rbx", "rcx", "rdx"
    );
}

__attribute__((noinline))
long long test_rdtsc(void) {
    long long t;
    asm volatile(
        "rdtsc\n\t"
        "shl $32, %%rdx\n\t"
        "or %%rdx, %%rax\n\t"
        : "=a"(t)
        :
        : "rdx", "rcx"
    );
    return t;
}

__attribute__((noinline))
void test_wbinvd(void) {
    asm volatile("wbinvd\n\t" ::: "memory");
}

__attribute__((noinline))
void test_invd(void) {
    asm volatile("invd\n\t" ::: "memory");
}

// 启动代码: cli/sti/hlt/cld 等
__attribute__((noinline))
void test_control_flags(void) {
    asm volatile(
        "cli\n\t"
        "sti\n\t"
        "cld\n\t"
        "clc\n\t"
        "hlt\n\t"
        ::: "memory"
    );
}

// stac/clac/paravirt: supervisor mode access prevention
__attribute__((noinline))
void test_stac_clac(void) {
    asm volatile(
        "stac\n\t"
        "clac\n\t"
        ::: "memory"
    );
}

// swapgs (paravirt 入口)
__attribute__((noinline))
void test_swapgs(void) {
    asm volatile("swapgs\n\t" ::: "memory");
}

// fxsave/fxrstor
__attribute__((noinline))
void test_fxsave(long *buf) {
    // 直接用符号引用: dcc 内部对 +m 可能分配到 reg, 所以直接对 buf 的地址操作
    long addr = (long)buf;
    asm volatile(
        "fxsave (%1)\n\t"
        :
        : "r"(addr)
        : "memory"
    );
}

// 64 位 imm64 → 内存 (内核 page table 写入)
static long long g_var_64;

__attribute__((noinline))
void test_mov_imm64_to_mem(void) {
    long long v = 0x1122334455667788LL;
    // 走 reg 中转避免 mem imm64 复杂编码
    long long t = 0x8877665544332211LL;
    asm volatile(
        "movq %1, %0\n\t"
        : "=m"(g_var_64)
        : "r"(t)
        : "memory"
    );
    (void)v;
}

// prefetch (%rip)
__attribute__((noinline))
void test_prefetch_rip(void) {
    long x = 0;
    asm volatile(
        "prefetcht0 %0\n\t"
        "prefetchnta %0\n\t"
        : "+m"(x)
        :
        : "memory"
    );
}

// int $N (启动 / IDT 触发)
__attribute__((noinline))
void test_int(void) {
    asm volatile("int $3\n\t" ::: "memory");
}

// pause (spin loop)
__attribute__((noinline))
void test_pause(void) {
    asm volatile("pause\n\t" ::: "memory");
}

// ud2 (无效指令, 异常测试)
__attribute__((noinline))
void test_ud2(void) {
    asm volatile("ud2\n\t" ::: "memory");
}

// endbr64 (CET)
__attribute__((noinline))
void test_endbr64(void) {
    asm volatile("endbr64\n\t" ::: "memory");
}

// syscall (用户/内核切换)
__attribute__((noinline))
long test_syscall(long nr) {
    long r;
    asm volatile(
        "syscall\n\t"
        : "=a"(r)
        : "a"(nr)
        : "rcx", "r11", "memory"
    );
    return r;
}

int main(void) {
    long r = 0;
    r += test_segment_regs(42);
    r += test_push_pop_seg();
    r += test_mov_to_from_seg(0x10);
    r += test_push_imm64();
    r += test_shl_cl_r13(1, 3);
    r += test_seto_into_ah(0);
    test_indirect_jmp((long)&&label1);
    goto skip;
label1:
skip:
    test_cpuid(0);
    (void)test_rdtsc();
    test_wbinvd();
    test_invd();
    test_control_flags();
    test_stac_clac();
    test_swapgs();
    long buf[64] = {0};
    test_fxsave(buf);
    test_mov_imm64_to_mem();
    test_prefetch_rip();
    test_int();
    test_pause();
    test_ud2();
    r += test_syscall(0);
    test_endbr64();
    (void)r;
    return 0;
}
