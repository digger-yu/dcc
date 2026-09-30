/* SSA 构建期别名事实：被取址的标量局部必须留在内存里，不能开 SSA 版本。
 *
 * 构建器早期把每个「以名字出现的左值」都当本函数局部去开 SSA 版本。对被取址的
 * 标量局部那是静默错值：`int acc=0; int *p=&acc; *p=5; return acc;` 里读 `acc`
 * 复用了指针写**之前**的版本，`-O2` 实测返回 0 而不是 5。这条与循环无关（该函数
 * 从来不含循环），却在「循环能否进 SSA」的讨论里被长期忽略 —— 因为它同时被
 * `func_has_loop` 之外的入口暴露出来，只有按档位跑一遍才看得见。
 *
 * 判据：`is_mem_only_object` = 「不是本函数的存储」∪「被取址的标量局部」；
 * 数组/结构体/联合体一律排除（它们的子访问本就按内存宽度走 IndexLoad/MemberLoad，
 * 把整个对象当一次标量 Load 会按错误宽度读内存）。
 *
 * 通过: exit 0（期望值取自 gcc -O0 参照）
 */
int counter;

int f_global_read(void);

/* 指针写 → 具名读 */
int f_ptr_write(void) {
    int acc = 0;
    int *p = &acc;
    *p = 5;
    return acc;
}

/* 具名写 → 指针读（反方向，同一条内存路径） */
int f_ptr_read(void) {
    int acc = 3;
    int *p = &acc;
    acc = 7;
    return *p;
}

/* 声明处初始化必须先落到内存，否则 `*p` 读到未初始化栈槽 */
int f_init_visible(void) {
    int acc = 41;
    int *p = &acc;
    return *p + acc;
}

/* 循环携带：读具名、经指针写 */
int f_loop_carry(void) {
    int acc = 0;
    int *p = &acc;
    int s = 0;
    for (int i = 0; i < 4; i++) {
        s += acc;
        *p = *p + i;
    }
    return s + acc;
}

/* 体内既读又写同一个被取址对象，且写的是 `*p` 形态 */
int f_indirect_sum(int n) {
    int acc = 0;
    int *p = &acc;
    int s = 0;
    while (n > 0) {
        int t = acc;
        s += t;
        *p = *p + n;
        n--;
    }
    return s + *p;
}

/* 自增/自减形态也必须读改写内存 */
int f_inc_taken(int n) {
    int k = 1;
    int *p = &k;
    k++;
    *p = *p + 1;
    n = k--;
    return n + k + *p;
}

/* 参数的地址同样逃逸 */
int f_param_taken(int n) {
    int *p = &n;
    n = n + 1;
    return *p + *p;
}

/* 无初值声明后直接经指针写入，再具名读出 */
int f_uninit_decl(void) {
    int acc;
    int *p = &acc;
    *p = 9;
    return acc;
}

/* 全局与同名局部并存：局部被取址不得影响全局的解析 */
int f_global_shadow(void) {
    counter = counter + 3;
    int counter = 100;
    int *p = &counter;
    *p = *p + 1;
    return counter * 10 + f_global_read();
}

int f_global_read(void) {
    return counter;
}

int main(void) {
    if (f_ptr_write() != 5)
        return 1;
    if (f_ptr_read() != 7)
        return 2;
    if (f_init_visible() != 82)
        return 3;
    if (f_loop_carry() != 10)
        return 4;
    if (f_indirect_sum(3) != 14)
        return 5;
    if (f_indirect_sum(0) != 0)
        return 6;
    if (f_inc_taken(0) != 7)
        return 7;
    if (f_param_taken(4) != 10)
        return 8;
    if (f_uninit_decl() != 9)
        return 9;
    if (f_global_shadow() != 1013)
        return 10;
    if (f_global_read() != 3)
        return 11;
    return 0;
}
