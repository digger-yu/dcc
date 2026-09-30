/* LICM 顺序夹具 ⑥：外提项与块内前置语句的相对顺序（根因 4）。
 *
 * `int t = gv2 + k + 5921782;` 是货真价实的不变式（gv2 是未被写的全局，k 在循环内
 * 既不读于 cond 也不被写），应当外提；但外提必须落在 `int k = 44556677;` **之后**、
 * while **之前**。旧实现是 `block.items = hoisted; items.extend(kept)`，等于把外提项
 * prepend 到所在块最前面 ⇒ t 在 k 初始化之前求值，读到的是未初始化栈槽，而且块内
 * 语句顺序一变，codegen 的栈槽分配与产物字节跟着变（"顺序单一真源"）。
 *
 * 两个魔数各服务一条断言：
 *   - 值本身由 gcc 参照锁定（错序时 sum 直接不对）；
 *   - `-S` 输出里 `$44556677` 必须先于 `$5921782` 出现（错序时先后颠倒）。
 */
int gv2 = 7;

int order_probe(int n) {
    int s = 0;
    int k = 44556677;
    while (n > 0) {
        int t = gv2 + k + 5921782;
        s = s + t + n;
        n--;
    }
    return s - k;
}

/* 同一块里有两个循环：第一个的外提项不得越过第二个的边界，也不得越过循环之间的
   那条 `k = k + 1;`。 */
int two_loops(int n) {
    int s = 0;
    int k = 3;
    while (n > 0) {
        int t = k * 100;
        s = s + t + n;
        n--;
    }
    k = k + 1;
    while (n > -3) {
        int u = k * 10;
        s = s + u + n;
        n--;
    }
    return s;
}

int main(void) {
    if (order_probe(2) != 56400258) return 1;
    if (order_probe(1) != 5921790) return 2;
    if (order_probe(0) != -44556677) return 3;
    /* two_loops(2)：第一轮 k=3 ⇒ t=300，s=(300+2)+(300+1)=603；n=0 结束第一循环，
       k=4；第二轮 u=40，n=0,-1,-2 ⇒ s += (40+0)+(40-1)+(40-2)=117 → 720。 */
    if (two_loops(2) != 720) return 4;
    if (two_loops(0) != 117) return 5;
    return 0;
}
