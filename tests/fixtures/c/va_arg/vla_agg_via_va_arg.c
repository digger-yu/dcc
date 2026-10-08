/* va_arg 取 VLA（运行时尺寸）结构体：成员基址与拷贝长度两处都错。
 *
 * 原夹具用 `struct {...} first = va_arg(ap, typeof(first));`，GCC 直接拒
 * （"variable-sized object may not be initialized except with an empty
 * initializer"），所以它从来没有 oracle，只在 dcc 上跑到 SIGSEGV —— 红灯无法
 * 归因。改成**先声明后赋值**（GCC 接受）后，同一个意图就有可编译的差分形式。
 *
 * 已定位的两处缺陷（D8）——**两者均已修**，本件已从 `va_arg_xfail/` 转正：
 *   1. 消费侧基址：`d.data[i]` 发的是 `lea -0x78(%r13),%rax`（VLA **元数据槽**
 *      的地址）当数组基址，应为 `mov -0x78(%r13),%rax`（槽里存的分配基址）。
 *      修在后端无关的 `semantics::address_from_storage`（VLA/VM 聚合归
 *      `StackSlotLoad`）+ `mem/object_resolver.rs::resolve_identity`（`vla_offsets`
 *      排在 `var_map` 前：同一个 VLA 对象同时在两张表里，旧顺序把它报成
 *      Local，身份就由类型快照推导 —— 那是「同一个对象两个家」的另一例）。
 *      只在 `d = va_arg(ap, __typeof__(d))` 之后出现；不含该赋值的同样声明
 *      （`int size = 5; struct {char data[size];} d; d.data[0]=7; ...`）以及
 *      `size` 为形参的版本都正确，所以触发面是"VLA 聚合经 va_arg 赋值"这一条。
 *   2. -O2/-O3 侧的崩溃与旧记录的「装载长度用回退 8」不是同一件事：复测时
 *      该档的拷贝长度已经在运行时求值（产物里 `lea 7(%r9),%rax; and $-8;
 *      rep movsb`），真因是函数体里有运行时尺寸对象时 SSA 往返丢了它的身份
 *      （基址落回描述符区、`sizeof` 读回 0）。现由
 *      `opt.rs::block_has_unsafe_ssa_pattern` 让带 VLA/VM 聚合的整函数退回 AST 路径。
 *
 * 判据用退出码：四档均期望 0（gcc 也是 0）。
 */
typedef __builtin_va_list va_list;

static int size;

static int probe(int n, ...) {
    va_list ap;
    __builtin_va_start(ap, n);
    struct {
        char data[size];
    } d; /* size 是运行时值 => VLA 聚合 */
    d = __builtin_va_arg(ap, __typeof__(d));
    __builtin_va_end(ap);
    for (int i = 0; i < size; i++)
        if (d.data[i] != (char) (i + 1))
            return i + 1;
    return 0;
}

int main(void) {
    size = 5;
    struct {
        char data[size];
    } a;
    for (int i = 0; i < size; i++)
        a.data[i] = (char) (i + 1);
    return probe(1, a);
}
