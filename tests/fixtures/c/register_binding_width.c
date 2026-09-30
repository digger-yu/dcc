/* `register asm` 绑定的读写必须与栈槽路径同一个宽度口径。
 *
 * 绑定路径原本是两次裸 64 位 `mov`：写入不按声明类型截断，读出不做符号/零扩展。
 * 后果是同一个变量在「绑到寄存器」和「放在栈槽」两种形态下给出不同的值 ——
 * 实测（修前 vs gcc，-O0/-O1 都不一致）：
 *
 *     register unsigned int v asm("rbx");  v = w;   // w = 0x1000000ABUL
 *     unsigned long bv = v;                 → 0         （应为 171）
 *     register unsigned int gvu asm("r15");         // file-scope 同一条路
 *     long gv = gvu;                        → 4294967467（应为 171）
 *
 * 只有「宽源赋给窄绑定变量」才暴露它：`c = 200` 这类写法在赋值处就已经把值转成了
 * 目标类型，寄存器里本来就是 -56，所以窄绑定日常用看着是对的。真正需要一起对齐的是
 * 截断（写入）与扩展（读出）两条口径 —— 二者现在共用 `Codegen::var_access_shape`，
 * 不再各算一遍。
 *
 * file-scope 的窄绑定额外钉住「类型必须跟着绑定表走」：这类变量在收集全局时就被
 * `continue` 掉，既不在 `var_map` 也不在 `globals_raw`，声明类型只能由绑定表携带。
 *
 * 控制组（`sv` / `ci` / 循环里的 `c`）修前修后都正确，留着是为了证明这次改动不是
 * 「把窄类型一律当 8 字节」，也不是「干脆不许绑定」。
 */
#include <stdio.h>

register unsigned int gvu asm("r15");
static long sink[4];

int main(void) {
    unsigned long w = 0x1000000ABUL;
    register unsigned int v asm("rbx");
    register signed int s asm("r12");
    register signed char c asm("r14");

    v = w;         /* 必须截断成 171（裸 64 位搬运会留下 0x1000000AB） */
    s = (long)-1;  /* 控制：源本来就是目标类型能容纳的值 */
    c = 200;       /* signed char 溢出 -> -56 */
    int ci = c;
    gvu = w;       /* file-scope 窄绑定同样要截断 */
    for (int i = 0; i < 3; i++) c += 1; /* 窄类型在 %r14 里反复读改：-56 + 3 = -53 */

    unsigned long bv = v;
    long sv = s;
    long gv = gvu;
    sink[0] = c;

    printf("bv=%lu sv=%ld ci=%d gv=%ld sink0=%ld\n", bv, sv, ci, gv, sink[0]);
    return (bv == 171 && sv == -1 && ci == -56 && gv == 171 && sink[0] == -53) ? 0 : 1;
}
