/* 混合符号整型比较的**公共类型**必须按 C11 6.3.1.8 判「有符号侧能否表示无符号侧全部值」,
 * 而宽无符号字面量（`0xffffffffUL`）的后缀无符号性必须撑过整条 SSA 往返。
 *
 * 这里钉的是四个独立站点（缺任何一个都修不好本文件的实测错值）:
 *
 *   ① `Type::usual_arith_type`（`src/ast.rs`）曾把「秩严格更高」直接当成结果留在有符号侧。
 *      LP64 上 `unsigned long`(秩 4) 与 `long long`(秩 5) **同为 64 位** —— `long long`
 *      表示不了 `unsigned long` 的 [2^63, 2^64) ⇒ 公共类型必须升到 `unsigned long long`。
 *      留在有符号侧 ⇒ 无符号比较拿到有符号条件码。这一条与优化档位无关: 实测
 *      `4294967295UL > -1LL` 在 **gcc 给 0、dcc -O0 也给 1**（错值来自类型推导本身）。
 *      `dispatch.rs` 的 Binop 臂当时还**另抄了一份**「只看秩」的秩表, 所以修一处不够。
 *
 *   ② SSA 的整型常量是**无类型**的（`Operand::Int(i128)`），重建时按量级阶梯猜类型 ⇒
 *      `0xffffffffUL` 被猜成 `long long`，无符号性在半路丢失。实测形态是把值先存进变量
 *      再比较（SCCP 把两侧都折成常量后只剩 `Operand::Int`），只有跑完 SSA 通道的档位才错。
 *      现在 `build` 期把前端写下的字面量类型记进 `Function::const_int_tys`，重建时优先读它；
 *      阶梯本身也修掉了「把装不进 `long long` 的正值挂成 `long long`」的谎报。
 *
 *   ③ 常量折叠器 `const_eval::cmp_binop_values` 自己又拍了一套符号性判据：「任一操作数
 *      自身类型是无符号 ⇒ 按无符号比较」（不是公共类型），而且它写的
 *      `f(a as u128 as i128, b as u128 as i128)` 在 Rust 里是**往返恒等**（负数原样带回来），
 *      「重解释为无符号」根本没生效 ⇒ `-1 > 0xffffffffUL` 被折成 0（gcc 给 1）。
 *      折叠在 **-O0 也会跑**（`optimize_with_full_target` 的 `level == 0` 提前返回排在
 *      `function_inline` 之后，而 `inline_expr` 末尾无条件调 `fold_expr`），所以这一条
 *      四档全错。两个错向必须一起修：只看「任一操作数无符号」会把 `-1L > 1U`
 *      （公共类型 `long`，应为 0）判成无符号，旧写法只是被往返恒等恰好抵消了。
 *
 *   ④ 「按值」记账盖不住小常量：`1` 既是 `int` 也是 `unsigned int`，同值异型 ⇒ 歧义
 *      ⇒ 回落到阶梯 ⇒ `int`。于是 `int x=-1; unsigned int y=1; return x>y;` 被 SCCP
 *      把**带类型的** `Var(y.1: UInt)` 换成**无类型的** `Int(1)` 后，重建只能猜回 `int`
 *      ⇒ 公共类型 `int` ⇒ 有符号条件码（实测 -O2/-O3 得 0，应 1；`DCC_SSA_OFF=sccp`
 *      时 IR 仍保留 `Var(x.1: Int) > Var(y.1: UInt)`，结果就正确）。现在比较指令的
 *      两个操作数类型在 `build` 期按 `dest` 名记进 `Function::cmp_operand_tys`，重建时
 *      用它给「裸 `Expr::Int` 操作数」补回类型。
 *
 * 判据: gcc -O0 与 dcc -O0/-O1/-O2/-O3 的 stdout 逐字节一致, 且退出码为 0
 * （退出码 = 本文件里**判错的比较条数**, 任何一条被编成另一种条件码都会同时改变
 * stdout 与退出码）。①③的形态在四档都错 ⇒ `check_opt_equivalence.sh`（拿 -O0 当基准）
 * 看不见它, 只能靠与 gcc 的对拍; ②④的形态只在 -O2/-O3 错, 两个脚本都能看见。
 *
 * 所有期望值均取自本机 gcc (-std=gnu2x -O0) 实测。
 */
#include <stdio.h>

/* —— ① 同宽混合符号: 公共类型必须是 unsigned long long (无符号比较) —— */

/* 秩高但同宽: long long 表示不了 unsigned long 的全部值 ⇒ 无符号胜。
 * 正确: 4294967295 > (unsigned long long)(-1) 为假 → 0; 只看秩则算成有符号: 4294967295 > -1 → 1。 */
static int ul_gt_ll_signed_neg(unsigned long u, long long s) {
    return u > s;
}

/* 实参顺序反过来（右操作数是有符号侧）必须同判。
 * 正确: (unsigned long long)-1 即 2^64-1, 它不小于 4294967295 ⇒ 0; 算成有符号则 -1 < 4294967295 ⇒ 1。 */
static int ll_lt_ul_signed_neg(long long s, unsigned long u) {
    return s < u;
}

/* `>=` 的反向形态: 编成有符号条件码时 0xffffffff 与 -1 的大小关系整个倒过来。 */
static int ul_ge_ll(long long s, unsigned long u) {
    return u >= s;
}

/* —— ① 的另一半: 有符号侧**确实更宽**时必须停在有符号类型 ——
 * 这条是反向探针: 把判据改成「混合符号一律无符号」也能过上面的用例, 但会在这里翻车。 */

/* long(8) 比 unsigned int(4) 宽 ⇒ 能表示 UINT_MAX 全部值 ⇒ 公共类型 long, 有符号比较。
 * 正确: -1 > 1u 为假 → 0; 误升 unsigned long ⇒ -1 变 huge → 1。 */
static int l_gt_ui(long l, unsigned i) {
    return l > i;
}

/* long long × unsigned int 同理停在有符号侧。 */
static int ll_lt_ui(long long s, unsigned i) {
    return s < i;
}

/* —— ② 宽无符号字面量的类型必须撑过 SSA —— */

/* 字面量直接进比较: `(unsigned long)x > 0xffffffffUL`。
 * x = -1 时正确值 32（(unsigned long)-1 是最大的无符号数）; 编成有符号条件码则 0。 */
static int wide_literal_direct(long x) {
    if ((unsigned long)x > 0xffffffffUL) {
        return 32;
    }
    return 0;
}

/* 同一个字面量先存进变量: SCCP 把 u 折成常量后, 两侧都是 `Operand::Int` ——
 * 这是无符号性最容易丢的那一步（只有跑完 SSA 通道的档位会错）。 */
static int wide_literal_via_var(long x) {
    unsigned long u = 0xffffffffUL;
    if ((unsigned long)x > u) {
        return 32;
    }
    return 0;
}

/* 三元结果里用同一判据（分支条件与结果类型都走 usual_arith_type）。 */
static int wide_literal_ternary(long x) {
    return (unsigned long)x > 0xffffffffUL ? 11 : 22;
}

/* 无符号侧与有符号侧都是变量、值落在高半轴: 公共类型 unsigned long long。
 * 注: 64 位加法在两种公共类型下**位模式相同** (2^64-1 与 -1 只差符号、加 1 都归 0),
 * 所以这条不区分值, 只钉「混合符号加法能过 `usual_arith_type`/`infer_binop_type` 且宽度
 * 仍是 64 位」这个形态 —— 能区分符号性的是上面那些比较。 */
static int mixed_high_half(unsigned long u, long long s) {
    return (int)(u + s);
}

/* 两侧都是**编译期常量**、且左侧的值落在 [2^63, 2^64): 量级阶梯曾把所有 ≥2^32 的值
 * 一律挂成 `long long`（包括它**装不下**的值）⇒ 两侧同为「有符号」⇒ ls==rs ⇒ 有符号
 * 比较。这个形态 patch ①（转换规则）修不了 —— 两侧类型看起来相同；只有②这一半
 * （不再谎报 + 优先读字面量类型）能修。实测：只修①时 -O0 给 1、-O2/-O3 给 0。
 */
static int both_const_high_half(void) {
    unsigned long u = (unsigned long)(long)-9;
    unsigned long w = 0xffffffffUL;
    return u > w;
}

/* 同上但无符号性只由 `UL` 后缀携带（左侧是变量的值传播而来，不是字面量）:
 * 重建时必须从 `const_int_tys` 查出 ULong，否则两侧一起被猜成有符号。 */
static int table_only_shape(long x) {
    unsigned long u = 0xffffffffUL;
    return (unsigned long)x > u;
}

/* —— ③ 两侧都是编译期常量: 折叠器必须在公共类型里比较 ——
 * 这些形态连 -O0 都会被打折（`function_inline` 里的 `inline_expr` 无条件跑 `fold_expr`）,
 * 所以它们能单独钉住 `cmp_binop_values` 那一处。 */

/* 公共类型 unsigned long: -1 变 2^64-1 ⇒ 真。旧折叠给 0。 */
static int fold_neg_gt_wide_lit(void) {
    return -1 > 0xffffffffUL;
}

/* 实参交换后同判（旧写法只看「任一侧无符号」，这一条它也能走对，但下面两条不行）。 */
static int fold_wide_lit_lt_neg(void) {
    return 0xffffffffUL < -1;
}

/* 同宽混合符号（long long × unsigned long ⇒ unsigned long long）: 应为真。 */
static int fold_ll_vs_ul_same_width_neg(void) {
    return -1LL > 1UL;
}

/* 折叠器的**反向**探针: long(8) 比 unsigned int(4) 宽 ⇒ 公共类型 `long` ⇒ 应假。
 * 旧代码的「任一操作数无符号」判据会选无符号域；它当时没翻车纯粹是因为
 * `as u128 as i128` 是往返恒等 —— 只修其中一半就会在这里错。 */
static int fold_l_gt_ui_neg(void) {
    return -1L > 1U;
}

/* 无符号减法产物的宽度: `0U - 1` 是 unsigned int 的 4294967295 ⇒ 真。 */
static int fold_uint_minus_one(void) {
    return 0U - 1 > 0;
}

/* ULONG_MAX vs LLONG_MAX: 公共类型 unsigned long ⇒ 真。 */
static int fold_max_ul_gt_max_ll(void) {
    return 18446744073709551615UL > 9223372036854775807LL;
}

/* —— ④ 小常量被传播后丢无符号性: 按值记账无法分辨 `1`(int) 与 `1`(unsigned) —— */

/* 两侧都是带常量初值的局部量: SCCP 后只剩两个 `Operand::Int`，
 * 公共类型本应是 `unsigned int` ⇒ 真；被猜成 `int` 就得假。 */
static int prop_int_vs_uint(void) {
    int x = -1;
    unsigned int y = 1;
    return x > y;
}

/* 同形但落在 64 位: `unsigned long` ⇒ 真。 */
static int prop_long_vs_ulong(void) {
    long x = -1;
    unsigned long y = 1;
    return x > y;
}

/* 同宽混合符号: long long × unsigned long ⇒ unsigned long long ⇒ 真。 */
static int prop_ll_vs_ulong(void) {
    long long x = -1;
    unsigned long y = 1;
    return x > y;
}

/* ④ 的反向探针: 有符号侧更宽 ⇒ 公共类型 `long` ⇒ 应假。 */
static int prop_signed_wins(void) {
    long x = -1;
    unsigned int y = 1;
    return x > y;
}

/* 同一条判据走「形参 + 常量」而不是「两个常量」: 比较的另一侧是运行时值，
 * 因此不会被整体折叠掉，钉的是发射的条件码符号性本身（不是折叠结果）。 */
static int prop_half_runtime(long x) {
    unsigned long y = 1;
    return x > y;
}

/* 逐条报 “gcc 预期 / 实际”；`bad` 同时进 stdout 与退出码。 */
static int bad;

static void chk(const char *tag, int got, int want) {
    if (got != want) {
        bad++;
    }
    printf("%-18s got=%-3d want=%-3d %s\n",
           tag, got, want, got == want ? "ok" : "MISMATCH");
}

int main(void) {
    /* —— ① 同宽混合符号 (只看秩会错) —— */
    chk("ul_gt_ll_neg", ul_gt_ll_signed_neg(0xffffffffUL, -1LL), 0);
    chk("ll_lt_ul_neg", ll_lt_ul_signed_neg(-1LL, 0xffffffffUL), 0);
    chk("ul_ge_ll_neg", ul_ge_ll(-1LL, 0xffffffffUL), 0);
    /* —— ① 的反向探针: 有符号侧更宽 ⇒ 必须停在有符号 —— */
    chk("l_gt_ui_neg", l_gt_ui(-1L, 1u), 0);
    chk("ll_lt_ui_neg", ll_lt_ui(-1LL, 1u), 1);
    /* —— ② 宽无符号字面量跨过 SSA 往返 —— */
    chk("lit_direct_pos", wide_literal_direct(1), 0);
    chk("lit_direct_neg", wide_literal_direct(-1), 32);
    chk("lit_direct_min", wide_literal_direct(-9223372036854775807LL - 1), 32);
    chk("lit_direct_max", wide_literal_direct(9223372036854775807LL), 32);
    chk("lit_via_var_neg", wide_literal_via_var(-1), 32);
    chk("lit_via_var_pos", wide_literal_via_var(1), 0);
    chk("lit_ternary_neg", wide_literal_ternary(-1), 11);
    chk("lit_ternary_pos", wide_literal_ternary(1), 22);
    chk("mixed_high_half", mixed_high_half(18446744073709551615ULL, 1LL), 0);
    /* 只有常量类型还原能修的两侧形态（转换规则单独不够）: */
    chk("both_const_high", both_const_high_half(), 1);
    chk("table_only_neg", table_only_shape(-1), 1);
    chk("table_only_pos", table_only_shape(0x100000000L), 1);
    /* —— ③ 常量折叠器（-O0 也错） —— */
    chk("fold_neg_gt_wide", fold_neg_gt_wide_lit(), 1);
    chk("fold_wide_lt_neg", fold_wide_lit_lt_neg(), 1);
    chk("fold_ll_ul_neg", fold_ll_vs_ul_same_width_neg(), 1);
    chk("fold_l_gt_ui_neg", fold_l_gt_ui_neg(), 0);
    chk("fold_uint_m1", fold_uint_minus_one(), 1);
    chk("fold_max_ul_ll", fold_max_ul_gt_max_ll(), 1);
    /* —— ④ 传播后的小常量丢无符号性（-O2/-O3 错） —— */
    chk("prop_i_u", prop_int_vs_uint(), 1);
    chk("prop_l_ul", prop_long_vs_ulong(), 1);
    chk("prop_ll_ul", prop_ll_vs_ulong(), 1);
    chk("prop_signed_wins", prop_signed_wins(), 0);
    chk("prop_half_neg", prop_half_runtime(-1), 1);
    chk("prop_half_one", prop_half_runtime(1), 0);
    chk("prop_half_max", prop_half_runtime(9223372036854775807LL), 1);
    printf("mixed_signedness_usual_arith: %d mismatch(es)\n", bad);
    return bad;
}
