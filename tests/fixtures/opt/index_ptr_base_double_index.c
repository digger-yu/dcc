/* 回归夹具: `&(ptr_array[0][k])` 的下标基址必须取**指针值**, 不是槽地址。
**
** `lower_lvalue_base_addr` (`src/ssa/build.rs`) 对任意内层 `Index` 都发 `IndexAddr`
** (取地址), 但那只对「结果类型是对象」(数组行 / 结构体 / 联合体) 成立。标量与指针
** 元素必须交出元素里的**值** —— `u32 **apWiData` 的 `apWiData[0]` 是 `u32 *` 值,
** `&(apWiData[0][k])` 的基址是那个指针值。旧实现少了一次取指针值的加载, 算出
** `apWiData + k*4` (槽地址数组内偏移), 写落进 `apWiData` 指针数组旁边的堆内存。
**
** 实测 sqlite `walCkptInfo` (`src/sqlite/wal.c`):
**   return (volatile WalCkptInfo*)&(pWal->apWiData[0][sizeof(WalIndexHdr)/2]);
** -O2 下 WalCkptInfo 的写踩坏堆 ⇒ `malloc(): invalid next size` 中止
** (-O0/-O1 与 gcc 均正常; 差分测试 T2 WAL)。结构体元素 (`&aDb[0].pBt`) 仍走
** IndexAddr, 否则元素被按值拷进帧临时, 写落进副本 (sqlite3BtreeOpen 段错误)。
**
** 对照形状 `struct_elem_addr` (结构体元素) 与 `row_addr` (数组行) 钉住另一侧。 */
typedef struct Ckpt {
    unsigned nBackfill;
    unsigned aReadMark[5];
    unsigned nBackfillAttempted;
} Ckpt;

typedef struct Db {
    int pgno;
    void *pBt;
} Db;

static unsigned *slot0; /* apWiData[0] 的指向目标由调用方给 */

/* 犯案形状: 指针数组元素的双重下标取址 (walCkptInfo)。 */
__attribute__((noinline)) static Ckpt *ckpt_of(unsigned **apWiData) {
    return (Ckpt *)&(apWiData[0][12]); /* 元素 12 = 字节偏移 48 */
}

/* 对照 1: 结构体数组元素的成员取址必须仍走 IndexAddr (不能按值拷贝)。 */
__attribute__((noinline)) static void db_set(Db *aDb, int i, void *bt) {
    aDb[i].pBt = bt;
}

/* 对照 2: 二维数组 (行类型) 的元素取址。 */
static int grid[3][4];

__attribute__((noinline)) static int *cell_at(int i, int j) {
    return &grid[i][j];
}

int main(void) {
    unsigned buf[32];
    unsigned *ptr = buf;
    Db dbs[2];
    Ckpt *c;
    int mask = 0;
    int i;

    for (i = 0; i < 32; i++)
        buf[i] = 0;
    dbs[0].pgno = 1;
    dbs[0].pBt = 0;
    dbs[1].pgno = 2;
    dbs[1].pBt = 0;

    /* 正确: 写进 buf[12..14]; 错误: 写进 &ptr 旁边的栈/堆内存。 */
    c = ckpt_of(&ptr);
    c->nBackfill = 0x11111111;
    c->aReadMark[0] = 0x22222222;
    c->aReadMark[4] = 0x33333333;
    if (buf[12] != 0x11111111)
        mask |= 1;
    if (buf[13] != 0x22222222)
        mask |= 2;
    if (buf[17] != 0x33333333)
        mask |= 4;

    /* 结构体元素成员写必须落到真对象上。 */
    db_set(dbs, 1, (void *)0xabcdef);
    if (dbs[1].pBt != (void *)0xabcdef)
        mask |= 8;
    if (dbs[0].pBt != 0)
        mask |= 16;

    /* 二维数组行寻址。 */
    grid[1][2] = 42;
    if (*cell_at(1, 2) != 42)
        mask |= 32;
    if (*cell_at(0, 0) != 0)
        mask |= 64;
    return mask;
}
