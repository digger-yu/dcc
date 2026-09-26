/* 跨编译器读 long double 变参：被调方把 16 字节 80-bit 槽当成 f64 读。
 *
 * SysV AMD64 下 `long double` 属 X87 类：变参实参**永远**放在
 * `overflow_arg_area`（16 字节对齐的 16 字节对象，内容是 `fstpt` 写出的真
 * 80-bit 编码），不进寄存器保存区。
 *
 * gcc caller 按上述规则放 `3.25L` → 槽里是 `00000000000000c8 0040`
 * （尾数 0xC800000000000000、指数字段 0x4000）。dcc 的
 * `gen_va_arg_long_double` 发的是 `movsd (%r9), %xmm0` —— 只取**低 8 字节**并
 * 当成 f64 位型，于是读到的恰好是那 64 位**尾数**（`c8 00 00 00 00 00 00 40`
 * 解释成 double），结果完全无关。实测症状：`(long long)a` 得到
 * -9223372036854775808（LLONG_MIN，即超范围 double 转整的饱和值）。
 *
 * 注意这一致性陷阱：dcc caller 与 dcc callee 之间**是自洽的**（两边都按
 * "8 字节 double + 8 字节零" 约定，见 abi/call.rs:541 的注释），所以单文件
 * 差分夹具查不出这个问题，只有混合链接才暴露。这也是本用例必须跨 TU 的原因。
 *
 * 修复的原子单元（三处必须同批，缺一不可）：
 *   1. caller：变参部分的 long double 必须 `fld qword [src]; fstp tbyte [槽]`
 *      写真 80-bit。注意不得与**具名形参**的 dcc 自定约定混为一谈（那条另案）。
 *   2. callee：named / global / ptr 三个 `gen_va_arg_long_double*` 变体都要改成
 *      `fld tbyte [槽]; fstp qword [scratch]; movsd xmm0, [scratch]`
 *      （计算仍按内部 f64 语义，只在跨界处转换）。
 *   只改 callee 会立刻破坏 dcc↔dcc：读 f64+零填充的槽做 `fld tbyte` 会拿到
 *   指数字段为 0 的次正规值。
 *
 * 判据：callee 把三个值写进全局，caller 校验并返回 0/1。期望退出码 0。
 */
typedef __builtin_va_list va_list;

long long AI, BI;
int AD;

int f(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    long double a = __builtin_va_arg(ap, long double);
    long double b = __builtin_va_arg(ap, long double);
    __builtin_va_end(ap);
    AI = (long long) a;
    BI = (long long) b;
    /* 小数首位：3.25 -> 2 */
    AD = (int) (a * 10.0L) - (int) AI * 10;
    return 0;
}
