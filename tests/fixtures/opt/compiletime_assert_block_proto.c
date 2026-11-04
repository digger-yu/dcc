/* 内核「编译期断言」惯用法夹具：块作用域 `extern` 原型 + 常量条件。
 *
 * 背景（`report/linux71-dcc-fulltree-build-2026-10-28.md` §3.19）：内核每一层编译期检查都是
 * 这个形状 —— `include/linux/compiler_types.h:668-678` 的 `__compiletime_assert`，经
 * `include/linux/build_bug.h:40` 的 `BUILD_BUG_ON_MSG` 展开：
 *
 *     do {
 *             __noreturn extern void __compiletime_assert_459(void)
 *                     __compiletime_error("...");
 *             if (!(condition)) __compiletime_assert_459();
 *     } while (0)
 *
 * 陷阱函数**故意不定义**，全靠编译器把常量条件的死臂折掉。实测全树 181 处
 * `undefined reference to '__compiletime_assert_NNN'`（外加同惯用法的
 * `__scoped_seqlock_invalid_target` / `__scoped_seqlock_bug` / `__BUILD_BUG_*` 一族）。
 *
 * 根因不在链接侧、也不在折叠器缺臂，而在**通道准入判据**：`opt.rs`
 * 的 `items_have_unsafe_ssa_pattern` 把「块里出现过 `Decl::Func`」一律当成 SSA 不能重建的
 * 形态（那条判据本意是有体的**嵌套函数定义**），于是整函数退出 SSA 通道 ⇒ SCCP 没跑 ⇒
 * `if (常量)` 连调用一起发进产物。逐形实测（判据 = 产物里是否还有指向陷阱的重定位）：
 * 原型写在文件作用域能折；写进同一个块 / 内层块 / 带 `__noreturn`+`__compiletime_error` /
 * 条件换成 `x == 0` / 外面多套一层 `do{}while(0)` 全不折（gcc 六形全折）。
 *
 * 两族符号**必须分开**，否则判据自相矛盾：
 *   * `trap_c*`：只出现在**常量条件**的死臂里 ⇒ -O1 以上连引用都不该有（产物级判据）。
 *   * `trap_r*`：由 `volatile` 读入的运行期条件把守 ⇒ 引用必须在、必须真的能跑
 *     （拦住「把活臂也折掉」的过修）。
 * 早先的版本让同一个符号既坐死臂又坐活臂，结果 `-O2` 里那条合法引用把判据顶成永远红，
 * 而运行期期望值还得手算 —— 两条都错。
 *
 * 判据为什么数 `.rela.text`：`elf64_all_rela` 只扫 `.rela.dyn/.plt/.iplt`，在 ET_REL 对象上
 * 永远是空集（旧的陷阱族用例就因此只测到「引用没发」的假一半）；调用站点在 `.rela.text`。
 */
#include <stdio.h>

static int hits;

/* --- 死臂专用：折叠后根本不该被引用 --- */
void trap_c1(void) { hits += 1; }
void trap_c2(void) { hits += 2; }
void trap_c3(void) { hits += 4; }
void trap_c4(void) { hits += 8; }
void trap_c5(void) { hits += 16; }
void trap_c6(void) { hits += 32; }
void trap_c7(void) { hits += 64; }

/* 与内核 `compiler_types.h:668-678` 同形的宏：原型在块里，条件是调用点写死的常量表达式 */
#define CTA(cond, fn)                                                                      \
    do {                                                                                   \
        extern void fn(void);                                                              \
        if (!(cond))                                                                       \
            fn();                                                                          \
    } while (0)

/* 形 1：宏展开 + 常量条件（内核最常见的一形） */
static void shape_macro_const(void)
{
    CTA(1, trap_c1);
}

/* 形 2：外面再多套一层 do { } while (0)（`BUILD_BUG_ON_MSG` 的包法） */
static void shape_nested_do(void)
{
    do {
        CTA(sizeof(long) == 8, trap_c2);
    } while (0);
}

/* 形 3：原型带 `__noreturn` + `__compiletime_error`（与内核逐字同形） */
static void shape_attributed_proto(void)
{
    do {
        extern __attribute__((noreturn)) void trap_c3(void)
            __attribute__((__compiletime_error__("trap_c3")));
        if (!(1))
            trap_c3();
    } while (0);
}

/* 形 4：条件换成关系运算而不是 `!` */
static void shape_relation(void)
{
    CTA(2 + 2 == 4, trap_c4);
}

/* 形 5：块内常量局部 + 块作用域原型（只依赖「能不能进 SSA 通道」，不依赖任何过程间信息） */
static void shape_local_const(void)
{
    extern void trap_c5(void);
    int y = 1;
    if (!y)
        trap_c5();
}

/* 形 6：原型在文件作用域、条件常量 —— 这条修前就能折，留着做同批对照 */
void trap_c6(void);
static void shape_file_proto(void)
{
    if (!(1))
        trap_c6();
}

/* 形 7：语句表达式初值 + 块内原型（`int t = ({...});` 这一族在内核宏里成串出现）。
 * 注：本形**至今仍未折**，而且不是块作用域原型的问题：`({...})` 初值本身就让函数进不了
 * SSA 通道（IR 表达不了语句表达式，`DCC_SSA_BAIL=1` 直接打出这一条），而 AST 层只代回
 * asm `"i"` 操作数、不代回普通读。试过把条件/选择子位置的普通读也纳入代回（一次就折），
 * 但它把 integration 里 10 条用例打回（含 `m5_bootstrap_selftest` 挂死），已回退。
 * 所以断言只盖到 `trap_c1..trap_c6`，本形的复现方法与缺口一并记在
 * `report/linux71-dcc-fulltree-build-2026-10-28.md` §3.23。*/
static int shape_stmt_expr_init(int keep)
{
    extern void trap_c7(void);
    int t = ({ 1; });
    if (!(t))
        trap_c7();
    return t + keep;
}

/* --- 活臂专用：运行期条件把守，引用必须在、也必须真跑 --- */
void trap_r1(void) { hits += 1000; }
void trap_r2(void) { hits += 2000; }

static void shape_runtime(int cond)
{
    extern void trap_r1(void);
    if (!(cond))
        trap_r1();
}

static void shape_runtime_invert(int cond)
{
    extern void trap_r2(void);
    if (cond == 0)
        trap_r2();
}

int probe(int which)
{
    volatile int v = which;
    int c = v;

    switch (which) {
    case 1: shape_macro_const(); break;
    case 2: shape_nested_do(); break;
    case 3: shape_attributed_proto(); break;
    case 4: shape_relation(); break;
    case 5: shape_local_const(); break;
    case 6: shape_file_proto(); break;
    case 7: (void)shape_stmt_expr_init(c); break;
    case 8: shape_runtime(c); break;
    case 9: shape_runtime(0); break;
    case 10: shape_runtime_invert(c); break;
    case 11: shape_runtime_invert(0); break;
    default: break;
    }
    return hits;
}

int main(void)
{
    int acc = 0;
    int i;

    for (i = 1; i <= 11; i++)
        acc += probe(i);
    /* 死臂一条都不该跑；活臂里 i=9 与 i=11 各跑一次 ⇒ 3000。
     * 期望值不硬编进退出码：两编译器**输出相同**才是判据（gcc 才是参照系）。 */
    printf("%d %d\n", acc, hits);
    return 0;
}
