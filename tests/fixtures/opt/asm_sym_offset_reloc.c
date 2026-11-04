/* 内联 asm 里 `sym±N` 形状的夹具（内核 cpufeature/ALTERNATIVE 那一类写法）。
 *
 * 背景（`report/linux71-dcc-fulltree-build-2026-10-28.md` §3.17）：全树链接曾报
 * 417 处 `undefined reference to 'boot_cpu_data+114'`，站点在 `.altinstr_aux+0x2` 与
 * `.init.text`。起因是内联 asm 的内存操作数文本 / `.pushsection` 里的数据表达式
 * 会把 **整串** `cpu_data+8` 当成一个符号名交下去；早期只在两条出口上剥过位移，
 * 而这类名字的来源有十一条 pending 列表、发射出口十来个，漏一条出口就是一片红。
 *
 * 两条都必须覆盖的形状：
 *   1. 代码里的 `movl cpu_data+8(%%rip), %0` —— 走 `pending_code_rels`（实测内核里
 *      `.init.text` 那 337 处就是这条，最早被修的 `pending_data_rels` 反而不是它）。
 *   2. `.pushsection .my_aux, "a"` 里的 `.quad cpu_data+8` —— 走 `pending_custom_sec_rels`
 *      （内核 `.altinstr_aux` 的形状）。
 *
 * 判据分两层，缺一不可：
 *   * 产物级：symtab 里不许出现**名字带 `+`** 的条目；`.rela.my_aux` 必须绑到本 TU
 *     有定义的 `cpu_data`（否则链接期就是 `undefined reference to 'cpu_data+8'`）。
 *   * 运行级：`cpu_data+8` 的**偏移必须真的生效**。只把名字剥对、把 `+8` 丢掉，
 *     产物级判据全绿但读出来是 `cpu_data[0]`（11）而不是 `cpu_data[1]`（22）——
 *     这正是「在解析侧拆名字」被否掉的理由（那十来个 `SymDisp(sym, _)` 发射点全部
 *     忽略第二个字段）。所以四档 (-O0..-O3) 的 stdout 必须与 gcc 逐字节一致。
 *
 * 注意 `%%rip`：模板里出现了操作数占位符 `%0`，GAS 的 `%` 必须写成 `%%`，否则
 * gcc 直接报 `invalid 'asm': operand number missing after %-letter`。
 */
#include <stdio.h>

unsigned long cpu_data[4] = {11, 22, 33, 44};

static int read_field(void)
{
    int v = -1;
    __asm__ __volatile__("movl cpu_data+8(%%rip), %0" : "=r"(v));
    return v;
}

static void emit_aux(void)
{
    __asm__ __volatile__(
        ".pushsection .my_aux, \"a\"\n\t"
        ".quad cpu_data+8\n\t"
        ".popsection\n"
    );
}

int main(void)
{
    emit_aux();
    /* 22 == cpu_data[1]：偏移丢了就会读到 11 */
    printf("%d\n", read_field());
    return 0;
}
