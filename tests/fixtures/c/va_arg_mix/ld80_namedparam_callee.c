/* 跨编译器传/收具名 long double 形参：槽内编码与返回寄存器两处不符。
 *
 * 本用例由 gcc caller 编译，dcc 编译 callee（`--mix`），另一方向同理。
 * 实测（三处缺陷全部修复后）：四个方向 —— gcc|gcc、dcc|dcc、dcc callee+gcc
 * caller、gcc callee+dcc caller —— 退出码均为 0。修复前的历史症状（dcc
 * callee + gcc caller → SIGSEGV(139)；gcc callee + dcc caller → 1）已由
 * `tests/integration/c_abi_vararg.rs::va_arg_mix_ld80_namedparam_cross_tu_gcc_interop`
 * 常驻钉住。
 *
 * GCC 真值（objdump 复核，勿用 dcc -S）：
 *   caller:  lea -0x10(%rsp),%rsp ; fstpt (%rsp)      每个实参 16 字节栈槽，
 *                                                         内容是**真 80-bit 编码**
 *   callee:  fldt 0x10(%rbp) ; fldt 0x20(%rbp)        从**栈**上读，不是 st(0)
 *   返回:    长浮点返回值确实在 st(0)（`fldt gld` 后 ret）
 *
 * ⚠ 更正一条曾写错的结论：SysV AMD64 下 x87 **参数走内存**，只有**返回值**用
 * st(0)。"具名 long double 形参应在 st(0)"是 i386 cdecl 的规则，曾被我写进
 * docs/longdouble_80bit_migration.md，已由上述 gcc 实测推翻。dcc 把形参放栈上、
 * 每参 16 字节这两点**本来就是对的**，错的只是槽内内容与入口读取方式。
 *
 * 三处独立缺陷（D10），需分别修：
 *   1. caller：具名位置的 long double 也须写 80-bit。`abi/call.rs` 现已有
 *      `movsd 高半 scratch → fld qword → fstp tbyte` 的正确序列，但被
 *      `ld_in_variadic` 限定只在变参位置生效（当时误判具名形参另有约定）。
 *      放开该判据即可，与已修的变参路径共用同一段编码。
 *   2. callee 入口：prologue 把 16 字节原样搬进局部槽（`emit/functions.rs`
 *      `stack_param_stores`，每个 long double 拆两个 8 字节 chunk），随后
 *      `movq -0x48(%r13), %xmm0` 把低 8 字节当 f64 读 —— 对 80-bit 编码即垃圾。
 *      须在入口降级：`fldt [入参槽]` → `fstpl [局部槽]` 并把局部槽高 8 字节清零
 *      （dcc 内部仍按"低 8 字节 f64 + 高 8 零"表示，见 `emit_ld80_narrow_to_xmm0`
 *      之外的 float.rs 注释）。
 *   3. 【已修】返回值。`abi/tyclass.rs:428` 的
 *      `needs_memory_return = classes_force_mem(abi_classes(ty))`，而
 *      `abi_classes(long double) == [X87]`，`classes_force_mem` 把
 *      `Memory | X87 | X87Up | ComplexX87` 归为同一类。但 SysV 不如此：
 *      X87 **标量**返回值在 **st(0)**（gcc 实测：`f` 的 `ret` 前是 `fldt gld`），
 *      只有*含* X87 成员的聚合才返回内存。后果：`f` 被按 sret 处理，callee 发
 *      `movq %xmm0,(%rdi)`（gdb 抓到确实死在此指令），而 gcc caller 不填 rdi
 *      → rdi 为垃圾 → SIGSEGV。
 *      修它需三处同改，缺一则 dcc↔dcc 立即崩：
 *        a) 把 X87 从 `classes_force_mem` 的**返回**判据剔出（不得同时影响
 *           参数传递侧 —— 参数确实走内存，就是刚修好的第 1/2 点）；
 *        b) callee 出口：内部 xmm0-f64 先落 scratch，再 `fld qword` 上 st(0)，
 *           必须在 `mov %r13,%rsp; pop %r13` 之前完成；
 *        c) caller 入口：调用后从 st(0) `fstp qword` 降级回 xmm0。
 *      消费点有两份：`emit/expr/call.rs:102` 与它在 `dispatch.rs:2118` 的影子
 *      副本，必须同步。-mno-sse 路径下值本就在 st(0)，需单独看，不能直接套用 b/c。
 *
 * 当前状态：第 1、2、3 点均已修复，四方向全绿。
 * 判据：callee 把两个形参的整数部分写进全局，caller 逐项判定并返回不同非零码。
 */
typedef __builtin_va_list va_list;

long long A, B;

long double f(long double a, long double b) {
    A = (long long) a;
    B = (long long) b;
    /* 返回值必须真的被人检查：上一轮正是因为旧夹具只读全局不读返回值，
     * 删掉错误的 sret 存储后四个方向全绿，但返回约定仍错 —— 典型的假绿。
     * 这里返回 a+b（=5.75），caller 逐项校验 st(0) 返回约定。 */
    return a + b;
}
