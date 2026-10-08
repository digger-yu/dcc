/* tests/fixtures/c/parallel_global_facts.c
 *
 * 「并行发射不得改产物」这条不变式的可执行形式，钉住三类**程序级事实**：
 *
 * 1. 全局变量的**声明类型**。`uc_tab` 是 `unsigned char[]`、`flag` 是 `_Bool`：
 *    读它们的那条 1 字节加载必须是 `movzbl`。并行 worker 重建 `Codegen` 时如果拿
 *    `Type::Int` 占位，同一条加载会变成 `movsbl`（§11.13 的 N12，与 N1 同一类：
 *    程序级事实没有随下行快照走）。
 * 2. **静态局部变量地图**。`counter` / `hist` 的 mangle 名与「实体在 data/bss」的
 *    分派都读 `static_local_map`；worker 里这张表是空的时，函数体会走「懒初始化 +
 *    帧内槽」那条退化路，字节比串行多一截（实测 `static_local_arrays.c`
 *    串行 1458B / 并行 1610B）。
 * 3. 全局**初值**。`seeded` 的初始化字节决定发射点能不能像串行那样折叠常量。
 *
 * 判据（`tests/reproducibility.rs::parallel_codegen_preserves_program_level_facts`）：
 * 同一份源 `-O2 -c` 在默认（并行）与 `DCC_NO_PARALLEL=1`（串行）下的 `.o`
 * **逐字节相同**。本夹具刻意不含字符串/浮点字面量，因此 N10（并行 `.rodata`
 * 内部化重复）不会干扰这条判据 —— 那才有资格要「整文件相同」而不是「只看 .text」。
 * 反向对照由用例自己做：它同时要求 `.text` 非空，避免「两边都没发代码」的假绿。
 */

unsigned char uc_tab[4] = {200, 1, 2, 3};
_Bool flag = 1;
int seeded = 7;

int use_globals(int i) {
    /* 全局的声明类型决定这条 1 字节加载的符号性 */
    int a = uc_tab[i & 3];
    int b = flag ? 1 : 0;
    int c = seeded + 1;
    /* 静态局部：名字 mangle 与「实体在 data/bss」都由程序级地图决定 */
    static int counter = 0;
    static long hist[3] = {1, 2, 3};
    counter += a + b + c;
    hist[i % 3] += counter;
    return counter + (int)hist[0];
}

int driver(int n) {
    int s = 0;
    for (int i = 0; i < n; i++) s += use_globals(i);
    return s + use_globals(1);
}
