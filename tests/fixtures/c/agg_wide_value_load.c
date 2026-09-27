/* agg_wide_value_load.c — 多 quad 聚合值装载: >16 字节聚合 rvalue 的每个字都必须落地
 *
 * 背景 (缺陷 8): 聚合 rvalue 的值模型是**缓冲地址** (sret 槽 / 物化槽), 而旧的
 * 聚合赋值目标被固定成「一个有帧偏移的具名变量」。于是 `s.f = g()` / `a[i] = g()`
 * / `*p = g()` 这类**运行时地址**目标根本进不到聚合路径, 落入恒 8 字节的标量
 * store —— 结果第一字被写成源缓冲的地址、其余字保留栈上残值 (「漏第二字」)。
 * C++ 协程 awaiter 帧槽 >8 字节失效正是这个根因的表面症状。
 *
 * 本夹具把「源形态 × 目标形态 × 尺寸」三条轴铺满, 每格都逐分量核对:
 *   目标: 局部具名 / 全局 / 结构体成员 / 数组元素 / 指针解引用 / 嵌套成员
 *   源:   Call 返回 / 聚合 lvalue / 复合字面量 / 赋值链 / va_arg 之外的整体拷贝
 *   尺寸: 16B (寄存器对返回) / 24B (含非整字余数 + 混合类) / 32B / 64B
 * 期望值由夹具自行核对并计入 `bad`, 末尾 `return bad`; 同一份源码另经 gcc -O0
 * 编译运行, stdout 与退出码逐字节对照 (见 integration.rs::agg_wide_value_load)。
 */
#include <stdio.h>

struct q2 { int a, b; long c; };         /* 16B: [Integer, Integer] 寄存器对 */
struct q3 { long a, b, c; };             /* 24B: 3 quad, 尾后有 0 字节余数 */
struct q4 { long a, b, c, d; };          /* 32B: Memory 类 (sret) */
struct q8 { long v[8]; };                /* 64B */
struct mix { long a; double b; long c; }; /* 24B: [Integer, Sse, Integer] */
struct holder { struct q4 f; long tail; };

static int bad;

/* 每个被检对象都填互不相同的十进制数字序列, 任一字丢失/错位都会显形 */
static void put4(const char *tag, long got0, long got1, long got2, long got3,
                 long w0, long w1, long w2, long w3) {
    if (got0 == w0 && got1 == w1 && got2 == w2 && got3 == w3) {
        printf("ok   %s %ld %ld %ld %ld\n", tag, got0, got1, got2, got3);
    } else {
        printf("BAD  %s got=%ld %ld %ld %ld want=%ld %ld %ld %ld\n",
               tag, got0, got1, got2, got3, w0, w1, w2, w3);
        bad++;
    }
}

struct q2 mk2(long k) { struct q2 r; r.a = (int)(k * 10 + 1); r.b = (int)(k * 10 + 2); r.c = k * 10 + 3; return r; }
struct q3 mk3(long k) { struct q3 r; r.a = k * 10 + 1; r.b = k * 10 + 2; r.c = k * 10 + 3; return r; }
struct q4 mk4(long k) { struct q4 r; r.a = k * 10 + 1; r.b = k * 10 + 2; r.c = k * 10 + 3; r.d = k * 10 + 4; return r; }
struct q8 mk8(long k) { struct q8 r; int i; for (i = 0; i < 8; i++) r.v[i] = k * 100 + i; return r; }
struct mix mkm(long k) { struct mix r; r.a = k * 10 + 1; r.b = (double)(k * 10 + 2); r.c = k * 10 + 3; return r; }

static struct q4 g4;
static struct q3 g3;
static struct q8 gg8;

int main(void) {
    /* --- 1. 局部目标 ← Call (32B, sret) --- */
    {
        struct q4 s;
        s = mk4(0);
        put4("local_call", s.a, s.b, s.c, s.d, 1, 2, 3, 4);
    }
    /* --- 2. 全局目标 ← Call (staged: 无帧偏移, 必须物化后整块拷) --- */
    g4 = mk4(2);
    put4("global_call", g4.a, g4.b, g4.c, g4.d, 21, 22, 23, 24);
    /* --- 3. 结构体成员目标 ← Call (运行时地址) --- */
    {
        struct holder h;
        h.tail = -1;
        h.f = mk4(3);
        put4("member_call", h.f.a, h.f.b, h.f.c, h.f.d, 31, 32, 33, 34);
        put4("member_tail", h.tail, h.f.a, 0, 0, -1, 31, 0, 0);
    }
    /* --- 4. 数组元素目标 ← Call --- */
    {
        struct q4 arr[3];
        arr[0].a = arr[0].b = arr[0].c = arr[0].d = -7;
        arr[2] = mk4(4);
        put4("elem_call", arr[2].a, arr[2].b, arr[2].c, arr[2].d, 41, 42, 43, 44);
        put4("elem_nbr", arr[0].a, arr[0].b, arr[0].c, arr[0].d, -7, -7, -7, -7);
        /* --- 5. 聚合 lvalue → 聚合 lvalue (24B) --- */
        {
            struct q3 u;
            u = mk3(5);
            g3 = u;
            put4("agg_copy", g3.a, g3.b, g3.c, 0, 51, 52, 53, 0);
        }
        /* --- 6. 指针解引用目标 ← Call --- */
        {
            struct q4 *p = &arr[1];
            *p = mk4(6);
            put4("deref_call", p->a, p->b, p->c, p->d, 61, 62, 63, 64);
            /* 经元素指针写入后再从数组视角读出 */
            put4("deref_view", arr[1].a, arr[1].b, arr[1].c, arr[1].d, 61, 62, 63, 64);
        }
        /* --- 7. 赋值链: `x = y = f()` 的值本身是聚合 --- */
        {
            struct q4 a, b;
            a = b = mk4(7);
            put4("chain_a", a.a, a.b, a.c, a.d, 71, 72, 73, 74);
            put4("chain_b", b.a, b.b, b.c, b.d, 71, 72, 73, 74);
        }
        /* --- 8. 64B 聚合 (8 quad) ← Call, 目标为全局 --- */
        gg8 = mk8(8);
        put4("g64_lo", gg8.v[0], gg8.v[1], gg8.v[2], gg8.v[3], 800, 801, 802, 803);
        put4("g64_hi", gg8.v[4], gg8.v[5], gg8.v[6], gg8.v[7], 804, 805, 806, 807);
    }
    /* --- 9. 16B 聚合走寄存器对返回 (不得被 >16B 路径改写而漏第二字) --- */
    {
        struct q2 v;
        v = mk2(9);
        put4("q2_reg", v.a, v.b, v.c, 0, 91, 92, 93, 0);
    }
    /* --- 10. 混合 ABI 类 (Integer/Sse/Integer) 24B ← Call --- */
    {
        struct mix m;
        m = mkm(1);
        put4("mix24", m.a, (long)m.b, m.c, 0, 11, 12, 13, 0);
        g3 = mk3(0);
        put4("g24", g3.a, g3.b, g3.c, 0, 1, 2, 3, 0);
    }
    /* --- 11. 复合字面量 → 运行时地址目标 --- */
    {
        struct holder h2;
        h2.f = (struct q4){111, 112, 113, 114};
        put4("member_lit", h2.f.a, h2.f.b, h2.f.c, h2.f.d, 111, 112, 113, 114);
        {
            struct q4 lit_arr[2];
            lit_arr[0] = (struct q4){121, 122, 123, 124};
            lit_arr[1] = lit_arr[0];
            put4("elem_lit", lit_arr[1].a, lit_arr[1].b, lit_arr[1].c, lit_arr[1].d, 121, 122, 123, 124);
        }
    }
    printf("bad=%d\n", bad);
    return bad;
}
