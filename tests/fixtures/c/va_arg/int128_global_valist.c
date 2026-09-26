/* 全局 va_list + __int128：多 eightbyte 对象不得跨 GP 区边界拆分，游标不得双推进。
 *
 * 覆盖缺陷 D3 / D4：
 *   D3 —— 全局 va_list 曾由 va_start 写真全局内存、va_arg 却读懒分配的函数私有
 *         栈槽（实测 SIGSEGV）；读写必须落在同一份内存上。
 *   D4 —— `__int128` 这类占 2 个 eightbyte 的对象，历史上先按 8 字节推进游标再
 *         整体落溢出区又推进一次（双推进），且在 gp_offset 只剩 8 字节时会把
 *         对象**拆开**：低半从寄存器保存区取、高半从溢出区取。SysV 与 GCC 的
 *         实际做法是"装不下就整体上栈，且 gp_offset 不推进"。
 *
 * `__int128` 一律由 mk() 在运行时构造，不用 128 位字面量：后者会撞上常量折叠
 * 丢失类型的独立缺陷，把两件事混在一个用例里会让红灯无法归因。
 *
 * 布局：callee(int n, ...) 的 gp_offset 起点为 8（1 个命名 GP 参数）。
 *   a: 8..24   寄存器保存区
 *   b: 24..40  寄存器保存区
 *   c: 40..56  越过 48 边界 → 整体入溢出区，gp_offset 保持 40
 *   d: 紧随 c 之后，从推进后的溢出区读，验证 c 的推进量恰为 16
 */
typedef __builtin_va_list va_list;

static va_list gap;
static long long ea_lo, ea_hi, eb_lo, eb_hi, ec_lo, ec_hi, ed;

static __int128 mk(long long lo, long long hi) {
    return ((__int128)hi << 64) | (unsigned long long)lo;
}
static long long lo_of(__int128 v) {
    return (long long)v;
}
static long long hi_of(__int128 v) {
    return (long long)(v >> 64);
}

static int reader(void) {
    __int128 a = __builtin_va_arg(gap, __int128);
    __int128 b = __builtin_va_arg(gap, __int128);
    __int128 c = __builtin_va_arg(gap, __int128);
    long long d = __builtin_va_arg(gap, long long);
    int r = 0;
    if (lo_of(a) != ea_lo || hi_of(a) != ea_hi) r |= 1;
    if (lo_of(b) != eb_lo || hi_of(b) != eb_hi) r |= 2;
    if (lo_of(c) != ec_lo || hi_of(c) != ec_hi) r |= 4;
    if (d != ed) r |= 8;
    return r;
}

static int callee(int n, ...) {
    __builtin_va_start(gap, n);
    int r = reader();
    __builtin_va_end(gap);
    return r;
}

int main(void) {
    /* 每个 128 位值的两半都可与 -1 / 0 区分，避免符号扩展与零值假绿。 */
    ea_lo = 0x0123456789abcdefLL;
    ea_hi = 0x0fedcba987654321LL;
    eb_lo = (long long)0xffff0000ff00ff00ULL;
    eb_hi = 0x7fffffffffffffffLL;
    ec_lo = (long long)0x8000000000000001ULL;
    ec_hi = (long long)0xffffffffffffffffULL;
    ed = 0x2468ace13579bdf0LL;

    __int128 a = mk(ea_lo, ea_hi);
    __int128 b = mk(eb_lo, eb_hi);
    __int128 c = mk(ec_lo, ec_hi);
    return callee(1, a, b, c, ed);
}
