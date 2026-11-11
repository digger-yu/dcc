/* 报告 §5.58：`Insn::bswap_reg` 的 B32 形态漏了 REX.B。
 *
 * `bswap r32/r64` 的寄存器号编在 **opcode 低 3 位 + REX.B**，没有 modrm —— 所以
 * r8–r15 在 B32 宽度下也只能靠 REX.B 表达（`41 0F C8` = `bswap %r8d`）。旧实现
 * 只在 B64 分支发 REX，目标是 r8 以上时静默编成 `0F C8`（`bswap %eax`）：**另一个
 * 寄存器**被翻转，真正持有操作数的那个号一个字节都没动，`__builtin_bswap32`
 * 于是原样返回入参。
 *
 * 内核现场是 fs/jbd2/journal.c 的 `journal_check_superblock`：
 *     sb->s_header.h_magic != cpu_to_be32(JBD2_MAGIC_NUMBER)
 * 右值（来自 `__swab32` 常量臂）落在 %r8，左值占着 %rax —— 正是"比较的左操作数
 * 在家、右操作数走被调用者保存号"这一形。bswap 编错后 `h_magic` 与未翻转的
 * 0xc03b3998 比较恒不相等 → "JBD2: no valid journal superblock found"，
 * ext4 根文件系统挂载失败（RT|ext4|FAIL:mount）。
 *
 * 判据按位（0 = 全对）：
 *   1 常量右值 vs 内存左值（内核同形，右值必落高号）
 *   2 运行时右值 vs 内存左值
 *   3 __builtin_bswap64 高号寄存器
 *   4 __builtin_bswap16（x86 无 bswap16，走 rol r16,8）
 *   5 常量左值 vs bswap 右值（角色对调）
 *   6 对照：返回值位置（低号 %rax），修复前就对
 *   7 十六进字面量带 u 后缀的直折（`0x11223344u`）
 *
 * 每个 bswap 的实参都带 Cast —— 解析期的字面量直折只认裸 Number，带 Cast 才会
 * 真正走到 codegen 的 `gen_builtin` 臂，也就是出问题的那条路。
 */

static unsigned g_be_magic = 0x98393bc0u; /* 磁盘上 BE 0xc03b3998 的 LE 读法 */
static unsigned g_rt_src;
static unsigned long long g_rt_src64;
static unsigned short g_rt_src16;

int main(void) {
    int rc = 0;

    /* 1. 内核同形：左值来自内存，右值是翻转后的常量。 */
    if (g_be_magic != (unsigned)__builtin_bswap32((unsigned)(0xc03b3998U)))
        rc |= 1;

    /* 2. 运行时右值：bswap(0xc03b3998) = 0x98393bc0 = g_be_magic。 */
    g_rt_src = 0xc03b3998u;
    if (g_be_magic != (unsigned)__builtin_bswap32(g_rt_src))
        rc |= 2;

    /* 3. bswap64 高号寄存器（REX.W|REX.B = 49 0F Cx）。 */
    g_rt_src64 = 0x0807060504030201ULL;
    if (0x0102030405060708ULL != (unsigned long long)__builtin_bswap64((unsigned long long)(g_rt_src64)))
        rc |= 4;

    /* 4. bswap16：rol r16, 8 路径，同一条 REX.B 判据。 */
    g_rt_src16 = 0x1234;
    if (0x3412 != (unsigned short)__builtin_bswap16((unsigned short)(g_rt_src16)))
        rc |= 8;

    /* 5. 角色对调：常量在左，bswap 在右。 */
    if (0xc03b3998u != (unsigned)__builtin_bswap32((unsigned)(0x98393bc0U)))
        rc |= 16;

    /* 6. 对照：结果直接返回（%rax），修复前也是对的。 */
    if ((unsigned)__builtin_bswap32((unsigned)(0x11223344U)) != 0x44332211u)
        rc |= 32;

    /* 7. 裸字面量带 u 后缀：走 parser 直折，旧 `eval_int_lit` 只给十进制摘后缀，
     * 十六进制的这里直接报 E0006（Number 和 `)` 已被吃掉）。 */
    if ((unsigned)__builtin_bswap32(0x11223344u) != 0x44332211u)
        rc |= 64;

    return rc;
}
