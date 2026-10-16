/* 回归夹具: 投机执行 (P14 specex) 的上提切片不得跨过「读写同一基名」的指令。
**
** `slice_is_movable` (`src/ssa/passes.rs`) 的三条屏障判据必须在**基名**上比:
** SSA→AST 重建按基名塌缩成可变量 (`p.0`/`p.1` 重建后都是变量 `p`), 跨过一条读写
** 同一基名的指令就会让那次读写看见另一个版本的值。旧判据拿 SSA 版本名比 (只在
** version==0 时恰好等于基名), 漏掉非零版本; 且只查了一个方向 (上提项读被跨过项
** 写的基名), 对称方向 (被跨过的项读上提项写的基名) 整个没查。
**
** 实测 sqlite `sqlite3GetVarint` (`src/util/util.c`  amalgamation):
**   a = ((u32)p[0])<<14;  b = p[1];  p += 2;  a |= *p;
** `b = p[1]` (读 p.0) 不在分支条件的依赖切片里, 被切片中 `p += 2` (写 p.1) 的
** 上提跨过, 塌缩后 `p[1]` 读到已自增的 p ⇒ 4 字节 varint 解码错。
** sqlite 症状: 18000+ 行 bulk insert 到 INTEGER PRIMARY KEY 表报
** "UNIQUE constraint failed" (rowid ≥ 16384 的 3/4 字节 varint 路径)。
**
** 本夹具是 sqlite3GetVarint 的 3/4 字节路径最小形状; 逐档与 gcc 对拍。 */
typedef unsigned char u8;
typedef unsigned int u32;
typedef unsigned long long u64;

#define SLOT_2_0 ((0x7f << 14) | (0x7f))

__attribute__((noinline)) static int getvarint(const u8 *p, u64 *v) {
    u32 a, b, s;

    (void)s;
    if (((signed char *)p)[0] >= 0) {
        *v = *p;
        return 1;
    }
    if (((signed char *)p)[1] >= 0) {
        *v = ((u32)(p[0] & 0x7f) << 7) | p[1];
        return 2;
    }

    a = ((u32)p[0]) << 14;
    b = p[1];
    p += 2;
    a |= *p;
    if (!(a & 0x80)) {
        a &= SLOT_2_0;
        b &= 0x7f;
        b = b << 7;
        a |= b;
        *v = a;
        return 3;
    }

    a &= SLOT_2_0;
    p++;
    b = b << 14;
    b |= *p;
    if (!(b & 0x80)) {
        b &= SLOT_2_0;
        a = a << 7;
        a |= b;
        *v = a;
        return 4;
    }
    *v = 0;
    return 9;
}

int main(void) {
    /* 1 字节: 0x05 */
    static const u8 b1[1] = {0x05};
    /* 2 字节: 0x81 0x7f -> 255 */
    static const u8 b2[2] = {0x81, 0x7f};
    /* 3 字节: 0x81 0x80 0x01 -> 16385 */
    static const u8 b3[3] = {0x81, 0x80, 0x01};
    /* 4 字节: 0x82 0x80 0x80 0x01 -> 4194305 (跨基名读写的犯案路径) */
    static const u8 b4[4] = {0x82, 0x80, 0x80, 0x01};
    u64 v = 0;
    int mask = 0;

    v = 0xdead;
    if (getvarint(b1, &v) != 1 || v != 5)
        mask |= 1;
    v = 0xdead;
    if (getvarint(b2, &v) != 2 || v != 255)
        mask |= 2;
    v = 0xdead;
    if (getvarint(b3, &v) != 3 || v != 16385)
        mask |= 4;
    v = 0xdead;
    if (getvarint(b4, &v) != 4 || v != 4194305)
        mask |= 8;
    return mask;
}
