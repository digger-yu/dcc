/*
 * 已定位未修（xfail）：`long double` **全局量**的读侧仍按 f64 取低 8 字节。
 *
 * 发射侧已经修好（`.data` 里现在是真 80-bit 编码，与 gcc 逐字节一致），
 * 但全局 lvalue 的 load 还发 `movsd`，于是把 80-bit 的**尾数字节**当成 f64
 * 位型读，再 `fstpt` 拓宽 → 打印成 `-nan`。两侧反汇编：
 *
 *   gcc : fldt   0x2edc(%rip)        # <h>       ← 读 10 字节
 *         fstpt  (%rsp)
 *   dcc : movsd  0x2159(%rip),%xmm0  # <h>       ← 只读低 8 字节且当 f64
 *         fstpt  (%rsp)
 *
 * 判据设计：退出码把"哪一项坏"编码进去，不依赖 stdout 比对（避免本仓库
 * 出现过的"只看退出码的假绿"与"含地址输出的假 MISMATCH"两类问题）。
 * 第 2、3 项是**对照项**：局部变量与字面量的同类读法已经正确，它们必须为真，
 * 否则说明修读侧时把已经对的路径改坏了。
 */
extern int printf(const char *, ...);

long double gh = 1.0L;
long double ga[2] = {2.5L, 3.0L};

int main(void)
{
    long double lh = 1.0L;          /* 对照：局部变量，读侧已修 */
    int r = 0;

    /* bit0: 全局量必须恰好等于 1.0 —— 当前为假（读到 -nan） */
    if (gh == 1.0L) r |= 1;
    /* bit1: 对照项，局部量应当正确（当前为真） */
    if (lh == 1.0L) r |= 2;
    /* bit2: 对照项，数组元素读法（当前为真） */
    if (ga[0] == 2.5L && ga[1] == 3.0L) r |= 4;
    /* bit3: 对照项，全局量参与运算不抛异常（当前为真，因为比较结果为假即可） */
    if (gh + 1.0L != gh) r |= 8;

    printf("r=%d want=15 gh=%.27Lf lh=%.27Lf ga0=%.27Lf\n", r, gh, lh, ga[0]);
    return r == 15 ? 0 : 1;
}
