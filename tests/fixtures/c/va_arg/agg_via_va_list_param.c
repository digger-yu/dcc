/* 通过已退化为指针的 va_list 形参读取聚合变参。
 *
 * 覆盖缺陷 D1：指针型 va_list 的寄存器保存区路径曾以 va_list 结构体自身
 * 为基址解引用（`mov r9,[rcx+rax]` 缺 REX.B），读回的是栈垃圾。聚合读取
 * 又只在 named 版本里写对过，所以 named 与 ptr 两条路径必须各自有回归用例。
 *
 * 不用 libc；每个判据一个 bit，避免 `sum - N` 形式的 mod-256 别名。
 */
typedef __builtin_va_list va_list;

struct Pair64 { long long a; long long b; };   /* size 16, classes [Integer, Integer] */
struct Mixed { int i; double d; };             /* size 16, classes [Integer, Sse] */

static struct Pair64 take_pair(int n, va_list ap) {
    (void)n;
    return __builtin_va_arg(ap, struct Pair64);
}

static struct Mixed take_mixed(int n, va_list ap) {
    (void)n;
    return __builtin_va_arg(ap, struct Mixed);
}

static int helper_pair(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    struct Pair64 p = take_pair(n, ap);
    __builtin_va_end(ap);
    int r = 0;
    if (p.a != 0x1122334455667788LL) r |= 1;
    if (p.b != 0x99aabbccddeeff01LL) r |= 2;
    return r;
}

static int helper_mixed(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    struct Mixed m = take_mixed(n, ap);
    __builtin_va_end(ap);
    int r = 0;
    if (m.i != 0x55aa33cc) r |= 4;
    /* 用整数域比较避免浮点表示差异: 4.25 精确可表示 */
    if (!(m.d > 4.0 && m.d < 4.5)) r |= 8;
    return r;
}

int main(void) {
    struct Pair64 arg;
    arg.a = 0x1122334455667788LL;
    arg.b = 0x99aabbccddeeff01LL;
    int r = helper_pair(1, arg);
    struct Mixed mm;
    mm.i = 0x55aa33cc;
    mm.d = 4.25;
    r |= helper_mixed(2, mm);
    return r;
}
