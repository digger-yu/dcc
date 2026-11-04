/* Test: __attribute__((malloc, alloc_size, assume_aligned,
 *         warn_unused_result, sentinel, format_arg)) */

/* malloc: returned pointer does not alias existing memory */
__attribute__((malloc))
void *my_alloc(unsigned long size);

/* alloc_size: argument 1 specifies allocation size */
__attribute__((malloc, alloc_size(1)))
void *my_malloc(unsigned long size);

/* alloc_size with two arguments: rows * cols */
__attribute__((malloc, alloc_size(1, 2)))
void *my_calloc(unsigned long n, unsigned long m);

/* assume_aligned: returned pointer aligned to 64 bytes */
__attribute__((malloc, assume_aligned(64)))
void *aligned_alloc(unsigned long size);

/* warn_unused_result */
__attribute__((warn_unused_result))
int check_permission(int uid);

/* sentinel: variadic function must be NULL-terminated */
__attribute__((sentinel))
int execlp(const char *file, ...);

/* format_arg: argument 1 is a format string */
__attribute__((format_arg(1)))
const char *my_gettext(const char *msgid);

/* Combined attributes */
__attribute__((malloc, alloc_size(1), assume_aligned(16), warn_unused_result))
void *combined_alloc(unsigned long size);

/* 只声明、本 TU **从不引用**的一个名字。它存在的唯一目的是给集成用例一个反向
 * 断言目标：GAS 在这种情况下不发未定义条目（实测 gcc 对本夹具只留被引用的那几条），
 * 而 dcc 以前会逐条登 UND——那个差异不只是浪费符号表，内核里它使
 * `hidden symbol 'vdso_u_arch_data' isn't defined` 直接堆死最终链接（报告 §3.25/§3.26）。
 * 拿它当对照比只删断言更硬：同一个对象里同时钉住「被引用必留」与「未引用必不存」。 */
int never_called_here(void);

typedef void *(*fn_t)(void);

/* 八个带属性的声明全部**取一次地址**放进 volatile 表：不执行任何东西（下面 main 只在
 * volatile 为 0 时用它们，运行结果与以前一致），但每个名字都真供了一个重定位。
 * 以前这个用例只调用 `my_malloc`，其余七个其实没被引用，所以它实际只验到了
 * 「属性被接受且没报错」；地址表把判据推到「外部链接真的还在」。 */
static volatile fn_t sinks[8];

int main(void) {
    void *p = my_malloc(128);
    (void)p;
    sinks[0] = (fn_t)my_alloc;
    sinks[1] = (fn_t)my_malloc;
    sinks[2] = (fn_t)my_calloc;
    sinks[3] = (fn_t)aligned_alloc;
    sinks[4] = (fn_t)check_permission;
    sinks[5] = (fn_t)execlp;
    sinks[6] = (fn_t)my_gettext;
    sinks[7] = (fn_t)combined_alloc;
    /* `never_called_here` 这里**不能出现**（上一版把它放在 `if (keep)` 臂里，等于
     * 自己把它引用了一遍，那条「未引用必须不存」的反向判据就永远不可能成立）。
     * 它只留文件作用域的那一条声明。 */
    return 0;
}
