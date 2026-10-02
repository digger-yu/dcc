/* Test: __seg_fs / __seg_gs (and friends) address space qualifiers.
 *
 * x86 segment-register address space qualifiers are a GCC extension used to
 * access memory through a non-default segment register (Linux kernel per-cpu
 * access goes through %gs:, TLS through %fs:).
 *
 * 契约（第 26 轮改过一次，这里写的是**当前**契约）:
 *   1. 解析: 限定符可出现在 decl-specifier、基类型与 `*` 之间、`*` 与名字之间,
 *      以及 typedef / typeof / cast / 结构体字段 / 函数指针声明符等位置; 六个段名
 *      (fs/gs/cs/ds/es/ss) 都接受 (gcc 只接受 fs/gs, dcc 更宽松 —— 见下面第 4 条)。
 *   2. 代码生成: 带段的 **pointee** 在解引用/下标/成员/inc-dec/聚合拷贝时都发段前缀
 *      (`%gs:` / `%fs:`), 与 gcc 一致。cs/ds/es/ss 记为 Generic, 不发前缀 —— 它们
 *      在 gcc 里根本不能用在这些位置, 没有可对拍的参照产物。
 *   3. 可执行性: 用户态 GS base == 0, 所以 `%gs:(p)` 与 `(p)` 指向同一地址, gs 那部分
 *      可以真跑; 而 **FS base != 0** (glibc TLS), `%fs:(p)` 会算到别的内存上去。
 *      因此本夹具里 fs 的那几**个函数只定义不调用** (它们仍会被发射到 asm 里,
 *      前缀断言照样看得见)。这是"哪些子表达式能进 main"的硬约束, 不是风格问题。
 *   4. 本夹具**不能**与 gcc 对拍产物: gcc 会直接拒绝其中几个位置
 *      (`__seg_gs int *f(void){ return &x; }` ⇒ "return from pointer to
 *      non-enclosed address space", `int * __seg_gs p` ⇒ "'__seg_gs' specified
 *      for parameter")。与 gcc 的执行级对拍放在
 *      `tests/fixtures/opt/seg_gs_access_matches_gcc_at_all_levels` 用的那份夹具里。
 *
 * 通过: 编译成功 + 前缀断言（见 tests/integration/c_lang.rs 的
 * `seg_qualifier_parses_all_positions_and_emits_segment_prefixes`）+ main 退出码 0。
 */

__seg_gs int gs_global = 0;
__seg_fs int fs_global = 0;

/* 1+2: decl-specifier 位置 + 函数返回类型 */
__seg_gs int *make_gs_ptr(void) {
    static int x = 42;
    return &x;
}

/* 3: 形参。fs 那一条用 gcc 也接受的位置（基类型与 * 之间），因为前缀断言
 * 要的是「pointee 带段 ⇒ 发 %fs:」。 */
int use_param_fs(int __seg_fs *p) { return *p; }
int use_param_gs(int __seg_gs *p) { return *p; }

/* 3b: `* ` 之后的段限定符位置（gcc 直接报错：'__seg_gs' specified for parameter）。
 * dcc 接受但不当成 pointee 空间 ⇒ 不发前缀。这里只覆盖“能编译”，不断言前缀。 */
int use_param_after_star(int *__seg_fs p) { return *p; }

/* 4+5: 基类型与 `*` 之间 + 局部变量 */
int local_test(int *p) {
    int __seg_gs *lp = p;
    return *lp;
}

/* 6: cast */
int cast_test(int *p) { return *(int __seg_gs *)p; }

/* 7: struct 字段 */
struct with_seg {
    __seg_gs int a;
    int __seg_fs *b;
};

/* 8: typedef */
typedef int my_int_t;
int typedef_test(int *p) {
    my_int_t __seg_gs *lp = (my_int_t __seg_gs *)p;
    return *lp;
}

/* 9: typeof */
int typeof_test(void) {
    __typeof__(int) __seg_gs *p;
    static int x = 99;
    p = &x;
    return *p;
}

/* 10: storage class combo (只声明, 不访问) */
static __seg_gs int static_gs_var;

/* 11: 多限定符链 */
int multi_seg(int *__seg_fs __seg_gs p) { return *p; }

/* 12: 与 const 组合 */
const int __seg_gs const_seg_var = 42;

/* 13: 全部六个 x86 段限定符。cs/ds/es/ss 不发前缀 (Generic), fs 发 %fs:,
 * gs 发 %gs: —— 反向对照就落在这几个函数上。 */
int test_cs(int *__seg_cs p) { return *p; }
int test_ds(int *__seg_ds p) { return *p; }
int test_es(int *__seg_es p) { return *p; }
int test_ss(int *__seg_ss p) { return *p; }

/* 13b: 完全不带段的对照函数 —— 断言里它必须一个前缀都没有, 否则"逢解引用就发前缀"
 * 也能把正向断言做绿。 */
int test_plain(int *p) { return *p; }

/* 14: 函数指针声明符里 * 与名字之间的段限定符 */
int *(*make_fnptr(int init))(void) {
    static int x;
    x = init;
    int *(* __seg_gs fn)(void) = 0;
    (void)fn;
    return 0;
}

/* 15: 函数指针返回类型 (在 ( 与 * 之间) */
int (*__seg_gs ret_fnptr(void))(int) {
    static int local = 99;
    (void)local;
    return 0;
}

/* 16: 函数指针字段声明符 */
struct with_fnptr {
    int *(* __seg_gs fp)(void);
};

/* 17: sizeof / _Alignof 里段限定符开头的类型 */
int test_sizeof_seg(void) { return sizeof(__seg_gs int); }
int test_alignof_seg(void) { return _Alignof(__seg_gs int); }
int test_sizeof_seg_ptr(void) { return sizeof(__seg_gs int *); }

/* 18: 经段限定指针的下标 / 成员 / 自增 / 赋值 —— 这些形状历史上只有 `*p` 会发前缀,
 * `p[i]`、`q->f` 静默按普通地址读写。 */
int idx_gs(int __seg_gs *p, int i) { return p[i]; }
void store_gs(int __seg_gs *p, int v) { *p = v; }
void idx_store_gs(int __seg_gs *p, int i, int v) { p[i] = v; }
int preinc_gs(int __seg_gs *p) { return ++*p; }

struct gs_fields {
    int __seg_gs *vp;
};

int arrow_gs(struct gs_fields *s) { return *s->vp; }

/* 19: 经「函数返回类型带段」的指针解引用 —— 这是旧实现里**唯一**能活到 codegen 的
 * 位置，留着它做错配对照（其他位置修之前不发前缀，这一条却发）。 */
int use_ret_gs_ptr(void) { return *make_gs_ptr(); }

/* 只有 gs 那几条能真跑 (用户态 GS base == 0)。fs 的那几个函数留到上面的"定义但
 * 不调用"部分, 断言在 asm 上完成。 */
int main(void) {
    int x = 5;
    int arr[2] = { 0, 0 };
    struct gs_fields gf;

    gs_global = 10;
    gf.vp = &x;

    store_gs(&x, 7);
    if (preinc_gs(&x) != 8) /* 7 -> 8, 表达式值是新值 */
        return 1;
    if (x != 8) /* 写必须真的落进对象 */
        return 2;
    idx_store_gs((int __seg_gs *)arr, 1, 3);
    if (idx_gs((int __seg_gs *)arr, 1) != 3)
        return 3;
    if (arrow_gs(&gf) != 8) /* *s->vp, vp 就是 &x */
        return 4;
    /* 段限定读与无限定读在用户态必须一致 (GS base = 0) */
    if (use_param_gs(&x) != 8 || local_test(&x) != 8 || cast_test(&x) != 8)
        return 5;
    if (typedef_test(&x) != 8 || typeof_test() != 99 || test_plain(&x) != 8)
        return 6;
    if (*make_gs_ptr() != 42)
        return 7;
    if (test_sizeof_seg() != 4 || test_alignof_seg() != 4 ||
        test_sizeof_seg_ptr() != (int)sizeof(void *))
        return 8;
    (void)fs_global;
    (void)static_gs_var;
    (void)multi_seg;
    (void)make_fnptr;
    (void)ret_fnptr;
    (void)const_seg_var;
    return 0;
}
