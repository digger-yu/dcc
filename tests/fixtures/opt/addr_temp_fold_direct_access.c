/* P13 阶段2 尾项：地址临时值豁免窗口（`inline_temps::addr_temp_folds_at_sole_memop` +
 * `expr_rebuild::fold_direct_named_access`）的行为锁。
 *
 * 放开的是**一种**父子形态：SSA 里 `AddrOf{base}` 的结果被紧邻的一条 `Store`/`Load`
 * 的地址操作数唯一使用 ⇒ 重建后是 `*(&sym) = v` / `x = *(&sym)` ⇒ 摘掉这层 deref，
 * 回到对 `sym` 的直接访问。收益实测：-O2 下 `int g; g = 1;` 从
 * `lea rax,[rip+g]; mov [r13-0x40],rax; mov rcx,rax; mov rax,1; movsxd; mov [rcx],eax`
 * 六条回到一条 `mov DWORD PTR [rip+g],eax`（那条落槽写从头到尾没人读过）。
 *
 * 这个夹具钉的是**反面**：所有不该折的形态一概不许折。每一段都对应豁免窗口里的一条
 * 判据，任何一条判据被放宽成「通用地址内联」就会在这里现形：
 *
 *   1. volatile 局部 —— `is_volatile` 门。那条通路要靠 `*(&v)` 保住访问次数与顺序；
 *   2. 指针值参与算术（`&a[2] - &a[0]`、`&x - &y`）—— `is_addr_producer` 的原始案底：
 *      内联进指针减法会把结果算成 0 / 负数 / 巨大垃圾；
 *   3. 地址被再套一层取址（`int **pp = &p` 之后 `**pp`）—— 父级是 `AddrOf`，不折；
 *   4. 地址作为实参进函数（`f(&a)`，被调方写它）—— 使用点是 Call，不折；
 *   5. 同一个地址被用两次（先 `*p1` 读再写）—— `use_count != 1`，不折；
 *   6. 定义与使用之间隔着别的指令（尤其是隔着一次会改基址归属的写）—— 非紧邻，不折；
 *   7. 取址逃逸的局部被「直接写」与「经指针写」交替访问 —— 折叠后两条通路必须仍指向
 *      同一块存储（这是本窗口唯一真正改变了发射形态、因而最可能静默错值的组合）；
 *   8. 循环里每轮都重新取址/写入 —— 折叠不能把循环不变量之外的时机带进来。
 */
#include <stdio.h>

static int bad;

static void chk(const char *name, long got, long want) {
    if (got != want) {
        printf("MISMATCH %s: got %ld want %ld\n", name, got, want);
        bad++;
    }
}

/* —— 折叠**应当**发生的形态：值必须与 gcc 逐档一致 —— */

static int gs_scalar;
static int gs_arr[4];

struct gs {
    int a;
    int b;
};
static struct gs gs_obj;

/* 段 0：标量对象写后读（全局，走 `named_addr_operand` 那条 AddrOf+Store）。 */
static long global_write_read(int seed) {
    gs_scalar = seed + 1;
    int seen = gs_scalar;
    gs_scalar = seen * 2;
    return gs_scalar - (seed + 1) * 2 + (seed + 1);
}

/* 段 1：volatile 局部。写必须保留，读回必须是那次写。 */
static long volatile_local(int seed) {
    volatile int v = seed;
    v = v + 3;
    v = v * 2;
    return v + seed;
}

static volatile int gv_shared;

/* 段 2：volatile 全局 + 普通全局交错（ volatile 门不得被相邻性带偏）。 */
static long volatile_shared(int seed) {
    gv_shared = seed;
    gs_scalar = seed + 7;
    gv_shared = gv_shared + gs_scalar;
    gs_scalar = gv_shared - gs_scalar;
    return gv_shared + gs_scalar;
}

/* 段 3：指针值算术。地址临时值必须继续是真实变量。 */
static long pointer_arith(int seed) {
    int base = seed;
    int *p0 = &gs_arr[0];
    int *p3 = &gs_arr[3];
    long gap = p3 - p0;
    long self_gap = &base - &base;
    gs_arr[0] = seed + 1;
    gs_arr[3] = seed + 2;
    return gap * 100 + self_gap + (gs_arr[0] + gs_arr[3]);
}

/* 段 4：地址被再套一层取址（`&p` 的使用点是普通赋值的右值，不是 Store/Load 的地址
 *        操作数）：必须继续保留真实的 `p`，经 `**pp` 写回的要能看到同一个对象。 */
static long nested_addr_of(int seed) {
    int inner = seed;
    int *p = &inner;
    int **pp = &p;
    inner = inner + 1;
    **pp = **pp + 2;
    return inner + *p + p[0];
}

/* 段 5：地址进函数（使用点是 Call）。 */
static void touch_through_ptr(int *q) {
    *q = *q + 5;
}
static long addr_into_call(int seed) {
    int a = seed;
    touch_through_ptr(&a);
    a = a + 1;
    return a;
}

/* 段 6：同一地址用两次（use_count != 1）。 */
static long addr_used_twice(int seed) {
    gs_scalar = seed;
    int *p = &gs_scalar;
    int first = *p;
    int second = *p;
    *p = first + second;
    return gs_scalar + first + second;
}

/* 段 7：定义与使用之间隔着一次写（非紧邻）。 */
static long addr_with_gap(int seed) {
    int a = seed;
    int b = seed + 1;
    int *pa = &a;
    b = b + 3;             /* 中间的写：窗口里不许把 a 的地址重算到这一点之后 */
    *pa = b + a;
    return a + b;
}

/* 段 8：取址逃逸的局部被「直接写」与「经指针写」交替访问。
 *        折叠只改发射形态，两条通路必须指向同一块存储。 */
static long escaped_alternating(int seed) {
    int a = seed;
    int *p = &a;
    a = a + 1;   /* 直接写 */
    int via_ptr = *p;   /* 经指针读：必须看到上一次直接写 */
    a = via_ptr + 2;   /* 直接写 */
    *p = *p + 4;       /* 经指针写 */
    return a + *p + via_ptr;
}

/* 段 9：循环里每轮重新取址/写入。 */
static long loop_escaped(int n) {
    int acc = 0;
    for (int i = 0; i < n; i++) {
        int v = i;
        int *p = &v;
        v = v + 1;
        acc += *p;
    }
    return acc;
}

/* 段 10：结构体标量成员（`lower.rs` 的 scalar_field 臂：整条左值取址 + Store）。 */
static long member_scalar(int seed) {
    gs_obj.a = seed;
    gs_obj.b = gs_obj.a + 1;
    int *p = &gs_obj.a;
    gs_obj.b = *p + 2;
    return gs_obj.a + gs_obj.b;
}

int main(void) {
    for (int s = -2; s <= 3; s++) {
        chk("global_write_read", global_write_read(s), s + 1);
        chk("volatile_local", volatile_local(s), (s + 3) * 2 + s);
        chk("volatile_shared", volatile_shared(s), (s + s + 7) + s);
        chk("pointer_arith", pointer_arith(s), 3 * 100 + 0 + (s + 1 + s + 2));
        chk("nested_addr_of", nested_addr_of(s), (s + 3) * 3);
        chk("addr_into_call", addr_into_call(s), s + 5 + 1);
        chk("addr_used_twice", addr_used_twice(s), (s + s) + s + s);
        chk("addr_with_gap", addr_with_gap(s), (s + 1 + 3 + s) + (s + 1 + 3));
        chk("escaped_alternating", escaped_alternating(s), (s + 1 + 2 + 4) + (s + 1 + 2 + 4) + (s + 1));
        chk("loop_escaped", loop_escaped(s + 5), (long) (s + 5) * (s + 6) / 2);
        chk("member_scalar", member_scalar(s), s + (s + 2));
    }
    printf("addr_temp_fold_direct_access: %d mismatch(es)\n", bad);
    return bad;
}
