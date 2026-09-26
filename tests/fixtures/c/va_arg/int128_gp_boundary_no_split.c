/* __int128 在 GP 保存区边界上不得被拆分，也不得双推进游标。
 *
 * 覆盖缺陷 D5：`gen_va_arg` 的 `__int128` 分支曾实现一条 GCC 里不存在的
 * "hybrid" 路径 —— `gp_offset == 40` 时把**低半留在寄存器保存区 [40]、高半从
 * overflow_arg_area[0] 取**，并且照样 `gp_offset += 16`。
 *
 * GCC 真值（从 `-O0` 输出里读到的判据是 `cmpl $39, gp_offset; ja 内存路径`）：
 *   - `gp_offset <= 39`（高半仍落在 GP 区最后一个槽 40..47 内）→ 两半都在保存区，
 *     `gp_offset += 16`；
 *   - 否则**整体**从 16 字节对齐后的 `overflow_arg_area` 取，`gp_offset` **不推进**，
 *     溢出游标 += 16。
 *
 * 用例把游标精确推到 40（起点 8 + 4 个 long long），然后取 `__int128` 再取一个
 * `long long`：第二项 `b` 的位置只有在"游标推进量正确"时才对得上，所以它能
 * 单独证伪"双推进/漏推进"，而 `a` 的两半能单独证伪"拆分"。
 */
typedef __builtin_va_list va_list;

static __int128 mk(long long lo, long long hi) {
    return ((__int128) hi << 64) | (unsigned long long) lo;
}
static long long lo_of(__int128 v) {
    return (long long) v;
}
static long long hi_of(__int128 v) {
    return (long long)(v >> 64);
}

static int probe(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    for (int i = 0; i < 4; i++) /* gp_offset: 8 -> 40 */
        (void) __builtin_va_arg(ap, long long);
    __int128 a = __builtin_va_arg(ap, __int128);
    long long b = __builtin_va_arg(ap, long long);
    __builtin_va_end(ap);
    int r = 0;
    if (lo_of(a) != 31 || hi_of(a) != 32)
        r |= 1; /* a 被拆分或读错区 */
    if (b != 44)
        r |= 2; /* 游标推进量错 */
    return r;
}

int main(void) {
    return probe(1, 10LL, 20LL, 30LL, 40LL, mk(31, 32), 44LL);
}
