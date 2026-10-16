/* 回归夹具: `||` / `&&` 右操作数带副作用或可陷获时必须整函数退出 SSA (builder 闸门)。
**
** SSA 通道不为短路求值造块: 整棵条件是一棵交给重建的表达式树, 右操作数会被**急切求值**
** 成块里的独立指令。判据见 `src/ssa/build.rs` 的 `Expr::Binop` lowering: 右操作数降级出
** 「不纯或可陷获」的指令 (store / load / 调用 / 除法) 就 `unsupported`, 与三元 `Cond`
** 降级出的 `Select` 同一条理由 (`emitted_is_pure`)。
**
** 实测 sqlite `walIndexPage` (`src/sqlite/wal.c`):
**   if( pWal->nWiData<=iPage || (*ppPage = pWal->apWiData[iPage])==0 )
** -O2 下即使 `nWiData<=iPage` 已短路, apWiData==NULL 的右操作数照样求值,
** `mov (%rax),%rax` 打空段错误 (-O0/-O1 与 gcc 均正常, WAL 模式下必崩)。
**
** 犯案形状 `lor_store_load`: 右操作数含指针写 + 可能打空的 load; 左操作数为真时
** 右操作数一次都不许发生。对照形状 `lor_pure` 右操作数是纯算术, 仍可进 SSA。 */
typedef struct Wal {
    int nWiData;
    unsigned *apWiData;
} Wal;

static int sink;

/* 左真时右操作数整个不许发生: apWiData 为空, 急切求值会打空。 */
__attribute__((noinline)) static int lor_store_load(Wal *pWal, int iPage, unsigned **ppPage) {
    if (pWal->nWiData <= iPage || (*ppPage = pWal->apWiData + iPage) == 0)
        return 1;
    return 0;
}

/* 右操作数纯算术: 不触发闸门, 行为不变 (钉住判据没有过度保守)。 */
__attribute__((noinline)) static int lor_pure(int a, int b, int *p) {
    if (a > 0 || (b * 3 + *p) > 100)
        return 1;
    return 0;
}

int main(void) {
    Wal w;
    unsigned *out = (unsigned *)0x1234;
    unsigned buf[4];
    int mask = 0;

    /* nWiData(0) <= iPage(0) 为真: 必须短路, 且 *ppPage 不被写。 */
    w.nWiData = 0;
    w.apWiData = 0;
    if (lor_store_load(&w, 0, &out) != 1)
        mask |= 1;
    if (out != (unsigned *)0x1234)
        mask |= 2;

    /* 左假时走右操作数: apWiData 非空, 写 *ppPage 并返回 0。 */
    buf[0] = 7;
    w.nWiData = 2;
    w.apWiData = buf;
    out = 0;
    if (lor_store_load(&w, 0, &out) != 0)
        mask |= 4;
    if (out != buf)
        mask |= 8;

    /* 纯右操作数两种走向。 */
    sink = 5;
    if (lor_pure(1, 2, &sink) != 1)
        mask |= 16;
    if (lor_pure(-1, 2, &sink) != 0)
        mask |= 32;
    return mask;
}
