/* va_arg 取 VLA（运行时尺寸）结构体：成员基址与拷贝长度两处都错。
 *
 * 原夹具用 `struct {...} first = va_arg(ap, typeof(first));`，GCC 直接拒绝
 * （"variable-sized object may not be initialized except with an empty
 * initializer"），所以它从来没有 oracle，只在 dcc 上跑到 SIGSEGV —— 红灯无法
 * 归因。改成**先声明后赋值**（GCC 接受）后，同一个意图就有可编译的差分形式。
 *
 * 已定位的两处缺陷（D8），objdump 复核：
 *   1. 消费侧基址：`d.data[i]` 发的是 `lea -0x78(%r13),%rax`（VLA **元数据槽**
 *      的地址）当数组基址，应为 `mov -0x78(%r13),%rax`（槽里存的分配基址）。
 *      只在 `d = va_arg(ap, __typeof__(d))` 之后出现；不含该赋值的同样声明
 *      （`int size = 5; struct {char data[size];} d; d.data[0]=7; ...`）以及
 *      `size` 为形参的版本都正确，所以触发面是"VLA 聚合经 va_arg 赋值"这一条。
 *   2. 装载长度：溢出区路径发的是 `mov $0x8,%rcx; rep movsb`，用的是编译期
 *      回退尺寸 8，而不是 VLA 的运行时尺寸。SysV 下这类对象必须由调用者按
 *      运行时大小整体传栈，`gen_va_arg_struct` 的 `sz` 需要换成运行时求值
 *      （与 `gen_aggregate_assign` 里 `dst_ty.has_vla()` 走 `vm_size_slot` /
 *      `gen_sizeof_runtime` 的做法一致）。
 *
 * 判据用退出码：期望 0。当前 dcc 为 SIGSEGV，GCC 为 0。
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
