/* 回归夹具: SSA→AST 重建把「已被另一条臂展开的汇聚块」当成不需要处理，于是兄弟臂
** 跳过汇聚块的全部语句 —— 汇聚块以 `Ret` 终结时丢掉的正是**返回值**：%eax 留着
** 上一次赋给别的东西的值当返回值。
**
** 触发形状是「两条臂各自又是一条 if/else-if 阶梯，且有一条内层臂提前 return」：
** `branch_join` 的快路径只沿 `Jump` 直链找汇聚块，阶梯的臂不是直链 ⇒ 给不出候选；
** 早退的 `Ret` 臂又使 `from` 没有真实后支配者（ipdom 只剩 EXIT 占位）⇒ `join=None`，
** 两条臂都自由展开，第一个到达汇聚块的臂把它内联掉并登记 `visited`，兄弟臂再到达时
** 直接返回空。重建侧的修复见 `src/ssa/rebuild.rs`（`Ret` 终结块没有后继，重复展开
** 有界，所以照常展开）。
**
** 形状取自 sqlite3.c 实测：`decodeFlags()` 就是这条阶梯，-O2 下它对内部页
** （flagByte==5）返回 11，于是 `btreeInitPage` 报「database disk image is malformed」,
** 凡是需要下内部页的多页库都读不出来。`member_ladder` 是它按结构体字段落事实的部分
** （`xCellSize`/`maxLocal` 那一族），`ladder_tail_stmt` 钉住「汇聚块里除 return 之外
** 的语句也不能丢」。参考实现写成 `switch` 形态：含 switch 的函数不进 SSA 通道，
** 于是它是独立的 AST 侧读数。 */

typedef struct Flags {
    int a;
    int b;
    int c;
} Flags;

/* 独立参考实现：与下面各被测函数语义相同，但用 switch 表达（不进 SSA 通道）。 */
__attribute__((noinline)) static int ref_ladder(int fb, int *o) {
    switch (fb) {
    case 13: *o = 2; return 0;
    case 10: *o = 3; return 0;
    case 2:  *o = 6; return 0;
    case 5:  *o = 7; return 0;
    default: *o = (fb >= 10) ? 4 : 8; return 11;
    }
}

__attribute__((noinline)) static int ladder(int fb, int *o) {
    if (fb >= 10) {
        *o = 1;
        if (fb == 13) {
            *o = 2;
        } else if (fb == 10) {
            *o = 3;
        } else {
            *o = 4;
            return 11;
        }
    } else {
        *o = 5;
        if (fb == 2) {
            *o = 6;
        } else if (fb == 5) {
            *o = 7;
        } else {
            *o = 8;
            return 11;
        }
    }
    return 0;
}

/* 汇聚块里除 `return` 之外还有语句：兄弟臂同样不能漏掉它们。 */
__attribute__((noinline)) static int ladder_tail_stmt(int fb, int *o) {
    if (fb >= 10) {
        if (fb == 13) {
            *o = 2;
        } else if (fb == 10) {
            *o = 3;
        } else {
            *o = 4;
            return 11;
        }
    } else {
        if (fb == 2) {
            *o = 6;
        } else if (fb == 5) {
            *o = 7;
        } else {
            *o = 8;
            return 11;
        }
    }
    *o += 100;
    return 0;
}

__attribute__((noinline)) static int ref_tail_stmt(int fb, int *o) {
    switch (fb) {
    case 13: *o = 102; return 0;
    case 10: *o = 103; return 0;
    case 2:  *o = 106; return 0;
    case 5:  *o = 107; return 0;
    default: *o = (fb >= 10) ? 4 : 8; return 11;
    }
}

/* 阶梯 + 三层深度 + 两种早退值（内部层也可能直接把汇聚块吃掉）。 */
__attribute__((noinline)) static int deep_ladder(int k, int *o) {
    if (k < 4) {
        if (k == 0) {
            *o = 1;
        } else if (k == 1) {
            *o = 2;
        } else if (k == 2) {
            *o = 3;
        } else {
            *o = 4;
            return 11;
        }
    } else {
        if (k == 4) {
            *o = 5;
        } else if (k == 5) {
            *o = 6;
        } else if (k == 6) {
            *o = 7;
            return 22;
        } else {
            *o = 8;
        }
    }
    return 0;
}

__attribute__((noinline)) static int ref_deep(int k, int *o) {
    switch (k) {
    case 0: *o = 1; return 0;
    case 1: *o = 2; return 0;
    case 2: *o = 3; return 0;
    case 3: *o = 4; return 11;
    case 4: *o = 5; return 0;
    case 5: *o = 6; return 0;
    case 6: *o = 7; return 22;
    default: *o = 8; return 0;
    }
}

/* 按字段落事实的那一族（decodeFlags 的 `pPage->xCellSize = ...` 形状）。 */
__attribute__((noinline)) static int member_ladder(Flags *p, int fb) {
    if (fb >= 10) {
        p->a = 1;
        if (fb == 13) {
            p->b = 2;
        } else if (fb == 10) {
            p->b = 3;
        } else {
            p->b = 4;
            return 11;
        }
    } else {
        p->a = 5;
        if (fb == 2) {
            p->b = 6;
        } else if (fb == 5) {
            p->b = 7;
        } else {
            p->b = 8;
            return 11;
        }
    }
    p->c = 99;
    return 0;
}

__attribute__((noinline)) static int ref_member(Flags *p, int fb) {
    switch (fb) {
    case 13: p->a = 1; p->b = 2; p->c = 99; return 0;
    case 10: p->a = 1; p->b = 3; p->c = 99; return 0;
    case 2:  p->a = 5; p->b = 6; p->c = 99; return 0;
    case 5:  p->a = 5; p->b = 7; p->c = 99; return 0;
    default: p->a = (fb >= 10) ? 1 : 5; p->b = (fb >= 10) ? 4 : 8; return 11;
    }
}

/* 阶梯在循环体内：循环模式下的展开路径同样要把每条臂的尾部 return 发出来。 */
__attribute__((noinline)) static int loop_ladder(const int *v, int n, int *acc) {
    int bad = 0;
    int i;
    for (i = 0; i < n; i++) {
        int o = 0;
        int r = 0;
        int oo = 0;
        int rr = 0;
        if (v[i] >= 10) {
            o = 1;
            if (v[i] == 13) {
                o = 2;
            } else if (v[i] == 10) {
                o = 3;
            } else {
                o = 4;
                r = 11;
            }
        } else {
            o = 5;
            if (v[i] == 2) {
                o = 6;
            } else if (v[i] == 5) {
                o = 7;
            } else {
                o = 8;
                r = 11;
            }
        }
        if (r == 0) {
            /* 汇聚块：只有走到这里的路径才有 *acc 的增量。 */
            *acc += o;
        }
        rr = ref_ladder(v[i], &oo);
        if (r != rr || o != oo)
            bad |= 1;
    }
    return bad;
}

int main(void) {
    int mask = 0;
    int i;
    for (i = 0; i < 16; i++) {
        int o = 0;
        int e = 0;
        int r = ladder(i, &o);
        int er = ref_ladder(i, &e);
        if (r != er || o != e)
            mask |= 1;
        o = 0;
        e = 0;
        r = ladder_tail_stmt(i, &o);
        er = ref_tail_stmt(i, &e);
        if (r != er || o != e)
            mask |= 2;
        {
            Flags p;
            Flags q;
            p.a = p.b = p.c = -1;
            q.a = q.b = q.c = -1;
            r = member_ladder(&p, i);
            er = ref_member(&q, i);
            if (r != er || p.a != q.a || p.b != q.b || p.c != q.c)
                mask |= 4;
        }
        if (i < 8) {
            int o2 = 0;
            int e2 = 0;
            r = deep_ladder(i, &o2);
            er = ref_deep(i, &e2);
            if (r != er || o2 != e2)
                mask |= 8;
        }
    }
    {
        static const int vec[16] = {0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15};
        int acc = 0;
        if (loop_ladder(vec, 16, &acc) != 0)
            mask |= 16;
        /* 只有 fb=2(6)、fb=5(7)、fb=10(3)、fb=13(2) 四条路径走到汇聚块。 */
        if (acc != 6 + 7 + 3 + 2)
            mask |= 32;
    }
    return mask;
}
