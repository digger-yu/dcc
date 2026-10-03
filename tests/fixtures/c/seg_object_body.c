/* Test: __seg_fs / __seg_gs 加在**对象本体**上（`__seg_gs int gv;` 这一形）。
 *
 * 与 `seg_qualifier.c` 的分工：那份夹具覆盖**限定符的语法位置**（pointee 那一侧），
 * 且其中有几个位置 gcc 直接拒绝，所以**不能**与 gcc 对拍。本夹具只放 gcc 完全接受
 * 的形状，因此既可以逐函数断言前缀，也可以与 gcc 四档做执行级对拍。
 *
 * 契约:
 *   1. 段限定符加在对象本体上（`__seg_gs int gv;` / `__seg_gs struct S gs;` /
 *      函数体内 `static __seg_gs int sgs;`）⇒ 对该对象的**每一笔真实内存访问**
 *      都必须带 `%gs:` / `%fs:` 前缀，-O0 与 -O2 两档都是。
 *   2. **取址不发前缀**（`&gv` ⇒ `lea gv(%rip), %rax`，与 gcc 同形）。硬件事实：
 *      LEA 不做内存访问，段重叠前缀对它是空操作 —— gas 会直接警告
 *      "segment override on `lea' is ineffectual"。前缀必须落在**访问**那一笔上
 *      （`gv++` 是 lea 取址 + 带前缀的读/写，见 `obj_inc_pre_stmt`）。
 *   3. 可执行性：用户态 GS base == 0 ⇒ `%gs:x(%rip)` 与 `x(%rip)` 同地址，gs 那部分
 *      可以真跑；**FS base != 0**（glibc TLS），`%fs:` 那几笔实测 SIGSEGV
 *      （gcc 与 dcc 的产物都是 rc=139，行为一致）。因此 fs 侧函数**只定义不调用**，
 *      断言全部在 asm 上完成。
 *   4. 反向对照：不带段限定的全局（含 `__seg_cs/ds/es/ss` 这组 Generic 段、指针
 *      变量本体、局部遮蔽全局的同名变量）必须**一个前缀也不发**。
 *
 * 通过: 逐函数前缀断言 + 与 gcc 四档执行结果一致 + main 自检 rc==0。
 * 断言位置: tests/integration/c_lang.rs
 *   `seg_qualifier_on_object_body_emits_segment_prefixes`
 */

struct S {
    int a, b;
};

/* ---- 段限定全局（gs 侧可执行） ---- */
__seg_gs int gv = 3;
__seg_gs int arr[4];
__seg_gs struct S gs;
__seg_gs long big[3];
__seg_gs unsigned char bytes[8];
__seg_gs double dv;
__seg_gs long double ldv;
static __seg_gs int sgs = 1;

/* ---- 段限定全局（fs 侧只定义不调用：FS base != 0） ---- */
__seg_fs int fv = 4;
__seg_fs int farr[4];
__seg_fs struct S fgs;
static __seg_fs int sfs = 1;

/* ---- 反向对照：不带段 ---- */
int gv_plain = 5;
int arr_plain[4];
struct S gs_plain;
long big_plain[3];
static int sp_plain = 1;

/* ---- 指针：AS 归 pointee，指针对象**本体**在默认段 ---- */
__seg_gs int *pseg = arr;
int *p_plain = arr_plain;

/* ================= 标量读 / 写 ================= */
int obj_rd(void) { return gv; }
void obj_wr(void) { gv = 99; }
int plain_rd(void) { return gv_plain; }
void plain_wr(void) { gv_plain = 6; }

/* ================= 取址：必须不发前缀（与 gcc 同形） ================= */
__seg_gs int *obj_take(void) { return &gv; }
int *plain_take(void) { return &gv_plain; }

/* ================= fs 侧读 / 写（asm 级断言） ================= */
int fs_rd(void) { return fv; }
void fs_wr(void) { fv = 8; }
int fs_static_rd(void) { return sfs; }
void fs_static_wr(void) { sfs += 3; }

/* ================= 复合赋值（8 种） ================= */
void obj_cad(void) { gv += 7; }
void obj_csub(void) { gv -= 2; }
void obj_cmul(void) { gv *= 3; }
void obj_cand(void) { gv &= 6; }
void obj_cor(void) { gv |= 1; }
void obj_cxor(void) { gv ^= 5; }
void obj_cmod(void) { gv %= 5; }
void obj_cshl(void) { gv <<= 2; }
void plain_cad(void) { gv_plain += 7; }

/* ================= 自增自减（前缀/后缀 × 弃值/取值） ================= */
void obj_inc_post_stmt(void) { gv++; }
void obj_inc_pre_stmt(void) { ++gv; }
void obj_dec_post_stmt(void) { gv--; }
void obj_dec_pre_stmt(void) { --gv; }
int obj_inc_post_val(void) { return gv++; }
int obj_inc_pre_val(void) { return ++gv; }
int obj_dec_post_val(void) { return gv--; }
int obj_dec_pre_val(void) { return --gv; }
void plain_inc(void) { gv_plain++; }

/* ================= 数组下标 ================= */
int arr_rd(int i) { return arr[i]; }
void arr_wr(int i, int v) { arr[i] = v; }
int arr_inc(int i) { return arr[i]++; }
void arr_cad(int i) { arr[i] += 3; }
long arr_rd_long(int i) { return big[i]; }
void arr_wr_long(int i, long v) { big[i] = v; }
unsigned char arr_rd_u8(int i) { return bytes[i]; }
void arr_wr_u8(int i, int v) { bytes[i] = v; }
int plain_arr_rd(int i) { return arr_plain[i]; }
void plain_arr_wr(int i, int v) { arr_plain[i] = v; }
void plain_arr_cad(int i) { arr_plain[i] += 3; }
long plain_arr_rd_long(int i) { return big_plain[i]; }
unsigned char plain_arr_rd_u8(int i) { return (unsigned char)arr_plain[i]; }

/* ================= 浮点 ================= */
double dbl_rd(void) { return dv; }
void dbl_wr(double v) { dv = v; }
long double ld_rd(void) { return ldv; }
void ld_wr(long double v) { ldv = v; }
int fs_arr_rd(int i) { return farr[i]; }

/* ================= 结构体成员 ================= */
int fld_rd(void) { return gs.b; }
void fld_wr(void) { gs.a = 6; }
void fld_cad(void) { gs.b += 4; }
void fld_inc(void) { gs.b++; }
__seg_gs struct S *fld_take(void) { return &gs; }
int plain_fld_rd(void) { return gs_plain.b; }
void plain_fld_wr(void) { gs_plain.a = 6; }
void plain_fld_cad(void) { gs_plain.b += 4; }

/* ================= 聚合：整体返回 / 初始化 / 赋值 ================= */
struct S agg_copy(void) { return gs; }
void agg_init(void) { struct S l = gs; l.a += 1; (void)l; }
void agg_assign(void) { gs = gs_plain; }
void agg_assign_rev(void) { gs_plain = gs; }
struct S plain_agg_copy(void) { return gs_plain; }
void plain_agg_init(void) { struct S l = gs_plain; l.a += 1; (void)l; }
void plain_agg_assign(void) { gs_plain = gs_plain; }

/* ================= static 局部带段（mangled 名也要发前缀） ================= */
int static_gs_rd(void) { return sgs; }
void static_gs_wr(void) { sgs += 3; }
int static_gs_inc(void) { return sgs++; }
int static_plain_rd(void) { return sp_plain; }
void static_plain_wr(void) { sp_plain += 3; }

/* ============ Generic 段（cs/ds/es/ss）的对象本体形状不在本夹具 ============
 * gcc 直接拒绝这四个限定符加在任何位置（`__seg_cs int x;` ⇒ “expected ‘;’”），
 * 没有可对拍的参照产物；那几个形状的反向对照放在 `seg_qualifier.c`。 */

/* ================= 指针：对象本体 vs pointee 两条轴 ================= */
void pseg_wr_obj(void) { pseg = 0; } /* 指针**本体**：默认段 ⇒ 不发前缀 */
int pseg_rd_pointee(void) { return *pseg; } /* pointee 带段 ⇒ 发前缀 */
void p_plain_wr_obj(void) { p_plain = 0; }
int p_plain_rd_pointee(void) { return *p_plain; }

/* ================= 局部遮蔽全局：不发前缀 ================= */
int shadow_gs(void) {
    int gv = 1;
    gv += 5;
    gv++;
    return gv;
}

/* ================= 组合形状 ================= */
int two(void) { return gv + arr[1]; }
void swap(void) {
    int t = gv;
    gv = gs.a;
    gs.a = t;
}

/* ================= 可执行自检 ================= */
/* 只调用 gs 侧与不带段的对照（fs 侧在用户态会算到 TLS 内存上去）。 */
int main(void) {
    gv = 1;
    if (obj_rd() != 1)
        return 1;
    obj_wr();
    if (gv != 99)
        return 2;
    obj_cad(); /* 106 */
    if (gv != 106)
        return 3;
    obj_csub();
    obj_cmul();
    obj_cand();
    obj_cor();
    obj_cxor();
    obj_cmod();
    obj_cshl();
    /* 106 -> 104 -> 312 -> (312&6)=0 -> 1 -> (1^5)=4 -> (4%5)=4 -> (4<<2)=16 */
    if (gv != 16)
        return 4;
    if (obj_inc_post_val() != 16)
        return 5;
    if (gv != 17)
        return 51;
    if (obj_inc_pre_val() != 18)
        return 6;
    if (gv != 18)
        return 61;
    obj_dec_post_stmt();
    if (gv != 17)
        return 7;
    obj_dec_pre_stmt();
    if (gv != 16)
        return 8;
    if (obj_dec_post_val() != 16)
        return 9;
    if (gv != 15)
        return 91;
    if (obj_dec_pre_val() != 14)
        return 10;
    if (gv != 14)
        return 101;
    if (gv != obj_rd())
        return 11;

    arr_wr(2, 20);
    if (arr_rd(2) != 20)
        return 10;
    if (arr_inc(2) != 20)
        return 11;
    if (arr_rd(2) != 21)
        return 111;
    arr_cad(2);
    if (arr_rd(2) != 24)
        return 12;
    arr_wr_long(1, 1234567890123L);
    if (arr_rd_long(1) != 1234567890123L)
        return 13;
    arr_wr_u8(3, 200);
    if (arr_rd_u8(3) != 200)
        return 14;

    dbl_wr(2.5);
    if (dbl_rd() != 2.5)
        return 15;
    ld_wr(1.25L);
    if (ld_rd() != 1.25L)
        return 16;

    gs.a = 7;
    gs.b = 9;
    if (fld_rd() != 9)
        return 17;
    fld_wr();
    if (gs.a != 6)
        return 18;
    fld_cad();
    fld_inc();
    if (gs.b != 14)
        return 19;
    struct S c = agg_copy();
    if (c.a != 6)
        return 20;
    if (c.b != 14)
        return 201;
    agg_init();
    gs_plain.a = 11;
    gs_plain.b = 12;
    agg_assign();
    if (gs.a != 11)
        return 21;
    if (gs.b != 12)
        return 211;
    agg_assign_rev();
    if (gs_plain.a != 11)
        return 22;
    if (plain_agg_copy().a != 11)
        return 23;
    plain_agg_init();
    plain_agg_assign();

    if (obj_take() != &gv)
        return 24;
    if (fld_take() != &gs)
        return 25;

    static_gs_wr();
    if (static_gs_rd() != 4)
        return 26;
    if (static_gs_inc() != 4)
        return 27;
    if (static_gs_rd() != 5)
        return 271;
    static_plain_wr();
    if (static_plain_rd() != 4)
        return 28;

    if (two() != gv + arr[1])
        return 29;
    gv = 30;
    gs.a = 40;
    swap();
    if (gv != 40)
        return 30;
    if (gs.a != 30)
        return 301;

    /* 不带段的全局：值必须照旧（同时 asm 侧断言一个前缀也不发） */
    plain_wr();
    if (plain_rd() != 6)
        return 31;
    plain_cad();
    plain_inc();
    if (gv_plain != 14)
        return 32;
    plain_arr_wr(1, 8);
    if (plain_arr_rd(1) != 8)
        return 33;
    if (plain_arr_rd_long(0) != 0)
        return 331;
    if (plain_arr_rd_u8(1) != 8)
        return 332;
    plain_arr_cad(1);
    if (plain_arr_rd(1) != 11)
        return 34;
    plain_fld_wr();
    gs_plain.b = 0;
    plain_fld_cad();
    if (plain_fld_rd() != 4)
        return 35;
    if (plain_take() != &gv_plain)
        return 36;

    pseg = arr;
    pseg[0] = 77;
    if (pseg_rd_pointee() != 77)
        return 39;
    pseg_wr_obj();
    if (pseg != 0)
        return 40;
    p_plain = arr_plain;
    p_plain[0] = 78;
    if (p_plain_rd_pointee() != 78)
        return 41;
    p_plain_wr_obj();
    if (p_plain != 0)
        return 42;

    if (shadow_gs() != 7)
        return 43;

    /* 反向：段限定读与无限定读在用户态（GS base = 0）必须一致 */
    gv = 5;
    if (obj_rd() != *(int __seg_gs *)&gv)
        return 44;
    return 0;
}
