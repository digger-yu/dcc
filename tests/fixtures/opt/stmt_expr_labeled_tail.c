/* N6 夹具：语句表达式「最后一个 item」带标签外壳时，值不能丢。
 *
 * GNU 语义里 `L: expr;` 的标签标的就是那条表达式语句，所以 `({ L: g(s); })` 的
 * 值就是 `g(s)`。旧实现只认裸 `Stmt::Expr`（`dispatch.rs` 里 `Expr::StmtExpr` arm
 * 的守卫写成 `BlockItem::Stmt(Stmt::Expr{..}) if i == last_idx`），于是带标签的最后
 * 一条落到「当普通语句发」那条丢弃结果的路径，dst 里剩的是**上一个操作数的残值**。
 *
 * 旧实测（修前，同一份源）：
 *   -O0/-O1：`a=(q+r)+({ skip: g(s); })` 得 5（= q+r，右操作数整个丢了），gcc 是 9；
 *            `b=t*({ skip2: g(s); })` 得 5（= t），gcc 是 20。
 *   -O2/-O3：两条都成垃圾（4211509 / 21057520）—— 残值来自更上游的寄存器内容。
 * 四档都坏，与寄存器分配无关，所以它不是分配器的债，是这条分类守卫的债。
 *
 * 反向对照（同一夹具里）：标签**被用到**时也不能坏。`goto done` 要求标号照样
 * 发射到符号表里，而取值路径又是另一条 —— 两者必须同时成立。
 */
#include <stdio.h>

static int calls;

static int g(int x) {
    calls += x;
    return x;
}

int main(void) {
    int q = 2, r = 3, s = 4, t = 5;
    int fail = 0;

    /* 1) 未被使用的标号 + 外层二元式两侧 */
    int a = (q + r) + ({ skip: g(s); });
    int b = t * ({ skip2: g(s); });
    if (a != 9) fail |= 1;
    if (b != 20) fail |= 2;

    /* 2) 标签在中间，最后一条仍是带标签的表达式语句 */
    int c = 1 + ({ head: g(1); mid: g(2); tail: g(3); });
    if (c != 4) fail |= 4;

    /* 3) 标号被 goto 用到：发射与取值两条路必须同时成立 */
    int v = 0;
    int d = 1 + ({ goto done; v = 5; done: v = 7; });
    if (d != 8) fail |= 8;
    if (v != 7) fail |= 16;

    /* 4) 多层标签外壳（`l1: ; l2: expr;`）也要剥到底 */
    int w = 0;
    int e = 2 + ({ l1: ; l2: w = 9; });
    if (e != 11) fail |= 32;
    if (w != 9) fail |= 64;

    /* 5) 「标签 ⟶ 复合语句」本身没有值可取：它不在最后一个位置，最后一条
     *    仍是表达式语句。这一条钉住「剥壳不能误伤非尾部的标签」（它们照旧
     *    由 `gen_stmt` 发射，不能因为尾部逻辑而被发两遍） */
    int x = 0;
    int f = 3 + ({ lb3: { x = 4; }; x; });
    if (x != 4) fail |= 128;
    if (f != 7) fail |= 256;

    printf("a=%d b=%d c=%d d=%d e=%d f=%d calls=%d\n", a, b, c, d, e, f, calls);
    return fail;
}
