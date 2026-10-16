/* 数组成员上的 `->` 必须带字段的真实类型（而不是退化成 `int`）。
 *
 * `type_of_member`（`src/parser.rs`）在 `is_arrow` 时只展开了 `Type::Ptr`：基址若是
 * **数组**（成员 `struct AuxDb a[3]`，或数组全局变量 `ga`），`a->z` ≡ `(*a).z`（数组
 * 退化成首元素指针），旧代码落进 `_ => int_t()` ⇒ 整个 `Member` 节点连同外层 `Assign`
 * 的类型被抹成 `int`。找字段用的是另一份同构逻辑（codegen 的 `member_target_ty`，
 * 它**会**展开数组），于是偏移正确、类型错误：
 *   - 64 位指针右值被按 `int` 左值「归约」成低 32 位符号扩展
 *     （`reduce_assign_value_to_lvalue`, `src/codegen/x86_64/emit/expr/int_ops.rs` 的
 *     `(4, true) => movslq`），高 32 位整段丢失；
 *   - sqlite3 shell 的 `main` 里那条 `p->pAuxDb` 形态的存储就是这么写坏，`open_db`
 *     随即 SIGSEGV（`movsbq (%rax)` 且 rax = 0xffffffffffffe999）。
 *
 * 判据：写入的指针都带非零高 32 位，读回值与「赋值表达式的值」逐档（-O0..-O3）与 gcc
 * 逐字节相同。只打印 `存储值 - 基址`，不打印裸地址 ⇒ 与 ASLR/链接方式无关。
 *
 * 同一族里 SSA 通道（-O2/-O3）另外两处也靠这份判据守住：
 *   - 成员写的基址不能按**右值**降级（聚合基址会被 `rep movsq` 拷进帧临时 ⇒ 丢写、
 *     读回 0），见 `src/ssa/lower.rs` 的 `lower_lvalue`；
 *   - `MemberLoad`/`MemberStore`/`MemberAddr` 的 `is_arrow`/`obj_ty` 跟着**基址操作数
 *     怎么物化**走，而不是跟着源码的 `.`/`->`，判据是
 *     `src/ssa/build.rs` 的 `member_base_is_ptr_operand`（形态 1/3/7 与形态 8 正好是
 *     它的两侧边界：数组/结构体基址要箭头化，指针基址必须保持取值形态）。
 */

#include <stdio.h>

struct AuxDb {
    void *db;
    const char *z;
    long extra;
    int iv;
};
struct Data {
    char pad[64];
    struct AuxDb a[3];
    char tail[64];
};

struct Data gd;
struct AuxDb ga[3];
char buf[64];

static unsigned long B(void) {
    return (unsigned long)buf;
}

/* 形态 1：全局结构体的数组成员 `gd.a->f`（旧路径：`Member.ty == int`） */
static void t_array_member(void) {
    gd.a->z = (char *)(B() + 0x100000005UL);
    gd.a->db = (void *)(B() + 0x200000007UL);
    gd.a->extra = (long)(B() + 0x300000009UL);
    gd.a->iv = 7;
    printf("member z=%lx db=%lx extra=%lx iv=%d\n",
           (unsigned long)gd.a->z - B(), (unsigned long)gd.a->db - B(),
           (unsigned long)gd.a->extra - B(), gd.a->iv);
}

/* 形态 2：数组全局变量 `ga->f`（Ident 的数组类型直接退化） */
static void t_array_ident(void) {
    ga->z = (char *)(B() + 0x40000000bUL);
    ga->extra = (long)(B() + 0x50000000dUL);
    printf("ident z=%lx extra=%lx\n",
           (unsigned long)ga->z - B(), (unsigned long)ga->extra - B());
}

/* 形态 3：箭头套箭头 `dp->a->f`（内层 `dp->a` 也是数组类型） */
static void t_arrow_of_arrow(void) {
    struct Data *dp = &gd;
    dp->a->z = (char *)(B() + 0x600000011UL);
    dp->a->db = (void *)(B() + 0x700000013UL);
    printf("arrow2 z=%lx db=%lx\n",
           (unsigned long)dp->a->z - B(), (unsigned long)dp->a->db - B());
}

/* 形态 4：赋值表达式的**值**（C11 6.5.16p3 取左值转换后的左操作数）也是全宽度指针 */
static void t_assign_value(void) {
    char *r = (gd.a[1].z = (char *)(B() + 0x800000017UL));
    void *r2 = (gd.a[1].db = (void *)(B() + 0x90000001dUL));
    long r3 = (gd.a[1].extra = (long)(B() + 0xa0000001fUL));
    printf("value r=%lx r2=%lx r3=%lx stored=%lx %lx %lx\n",
           (unsigned long)r - B(), (unsigned long)r2 - B(), (unsigned long)r3 - B(),
           (unsigned long)gd.a[1].z - B(), (unsigned long)gd.a[1].db - B(),
           (unsigned long)gd.a[1].extra - B());
}

/* 形态 5：复合赋值的值同样要全宽度（`a->extra += k` 在指针位型上算） */
static void t_compound(void) {
    gd.a[2].extra = (long)(B() + 0x100000000UL);
    long v = (gd.a[2].extra += 0x11ULL);
    gd.a[2].z = (char *)(B() + 0x200000000UL);
    gd.a[2].z += 0x22;
    printf("compound v=%lx z=%lx\n", (unsigned long)v - B(),
           (unsigned long)gd.a[2].z - B());
}

/* 形态 6：局部结构体 + 退化成指针后的常规箭头（对照组，走 Ptr 分支） */
static void t_local(void) {
    struct Data ld;
    ld.a[0].z = (char *)(B() + 0x300000023UL);
    struct AuxDb *p = ld.a;
    p->extra = (long)(B() + 0x400000029UL);
    p[1].z = (char *)(B() + 0x50000002fUL);
    printf("local z=%lx extra=%lx p1=%lx\n",
           (unsigned long)ld.a[0].z - B(), (unsigned long)p->extra - B(),
           (unsigned long)p[1].z - B());
}

/* 形态 7：循环里按下标与箭头混写（高档位下地址在循环体外提出时也不能改类型） */
static void t_loop(void) {
    for (int i = 0; i < 3; i++) {
        gd.a[i].db = (void *)(B() + 0x100000000UL * (unsigned long)(i + 1));
    }
    (gd.a + 2)->z = (char *)(B() + 0x600000031UL);
    printf("loop db0=%lx db1=%lx db2=%lx z2=%lx\n",
           (unsigned long)gd.a[0].db - B(), (unsigned long)gd.a[1].db - B(),
           (unsigned long)gd.a[2].db - B(), (unsigned long)(gd.a + 2)->z - B());
}

/* 形态 8：基址类型是**指针**的下标 `pa[i]->f` —— 这一类不能地址化
 *
 * `struct AuxDb *pa[2]` 里 `pa[i]` 的类型是 `Ptr(AuxDb)`（标量），`lower_expr` 交回的
 * 就是槽里那个指针**值** —— 正是箭头形态要的操作数。把它跟聚合基址混为一谈、发
 * `IndexAddr` 取**槽地址**再当对象指针，字段偏移就落在指针槽自己身上（实测 -O2/-O3 下
 * `gpa[0]->iv` 读回的是栈垃圾）。判据：`member_base_is_ptr_operand` 只对数组/结构体/
 * 联合类型成立。
 */
struct AuxDb ta[3];
struct AuxDb *gpa[3];

static void t_ptr_base_index(void) {
    struct AuxDb la[2];
    struct AuxDb *lpa[2];
    lpa[0] = la;
    lpa[1] = la + 1;
    gpa[0] = ta;
    gpa[1] = ta + 1;
    gpa[2] = ta + 2;
    for (int i = 0; i < 2; i++) {
        lpa[i]->z = (char *)(B() + 0x70000003bUL + (unsigned long)i);
        lpa[i]->extra = (long)(B() + 0x800000041UL + (unsigned long)i);
    }
    long v = (lpa[1]->extra += 0x10);
    printf("ptrbase v=%lx z0=%lx z1=%lx e0=%lx\n",
           (unsigned long)v - B(), (unsigned long)lpa[0]->z - B(),
           (unsigned long)lpa[1]->z - B(), (unsigned long)lpa[0]->extra - B());

    long *pe = &gpa[2]->extra;
    *pe = (long)(B() + 0x900000043UL);
    void *r = (gpa[0]->db = (void *)(B() + 0xa00000047UL));
    gpa[0]->iv = 32;
    gpa[1]->z = (char *)(B() + 0xb0000004bUL);
    printf("ptraddr *pe=%lx stored=%lx r=%lx db0=%lx iv=%d z1=%lx\n",
           (unsigned long)*pe - B(), (unsigned long)ta[2].extra - B(),
           (unsigned long)r - B(), (unsigned long)ta[0].db - B(),
           gpa[0]->iv, (unsigned long)gpa[1]->z - B());
}

int main(void) {
    t_array_member();
    t_array_ident();
    t_arrow_of_arrow();
    t_assign_value();
    t_compound();
    t_local();
    t_loop();
    t_ptr_base_index();
    return 0;
}
