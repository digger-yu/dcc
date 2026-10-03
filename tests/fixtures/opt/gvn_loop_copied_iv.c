/* AST 层 GVN 的 coalesce 不得删掉「循环变量的一次性拷贝声明」。
 *
 * `coalesce_block` 把 `int b = a;`（a 之后不再被改）合并成别名 `b`→`a` 并**删掉
 * b 的声明**。旧实现的「a/b 之后再没被写过」只看**当前块**的写入集合，而
 * `for (int i = ti; i < 5; i++)` 的声明位于一个只含该声明的子块（for-init）里，
 * 写 `i` 的 `i++` 属于**外层** `Stmt::For` 节点 —— 块内视角完全看不到它。
 * 于是声明被删，循环读一个从未赋值的 `i`：实测
 * `for(int ti=0;ti<5;ti=ti+4){for(int i=ti;i<5;i++)s=s+i;}` 在 `-O2` 得 9 而应 14。
 * 同一根因的另一形态（P10 分块的产物形态，4 层嵌套 + `&&` 边界）在 -O2 得 0 而应 50。
 *
 * 判据：coalesce 的写入集合必须是**全函数**视角（含外层循环 step/cond、兄弟块、
 * switch 体），见 `ast_gvn::FnScope::assigned`。
 *
 * 通过: exit 0（期望值取自 gcc -O0 参照）
 */

/* 内层循环变量由外层循环变量初始化，外层步长是复合赋值 */
int tile_like(int n) {
    int s = 0;
    for (int ti = 0; ti < 5; ti = ti + 4) {
        for (int i = ti; i < 5 && i < ti + 4; i++) {
            s = s + i;
        }
    }
    return s;
}

/* 三层同形 */
int tile3(int n) {
    int s = 0;
    for (int ti = 0; ti < 5; ti = ti + 4) {
        for (int tj = 0; tj < 5; tj = tj + 4) {
            for (int i = ti; i < 5 && i < ti + 4; i++) {
                s = s + i + tj;
            }
        }
    }
    return s;
}

/* 内层变量在体内被读两次 + 体内另有累加 */
int tile_body(int n) {
    int s = 0;
    for (int ti = 0; ti < 5; ti = ti + 4) {
        for (int i = ti; i < 5 && i < ti + 4; i++) {
            int a = i + i;
            s = s + a + i;
        }
    }
    return s;
}

/* 普通一次性拷贝仍应被正常处理（不被误删语义）：b 是 a 的副本，a 之后再没改 */
int plain_copy(int n) {
    int a = 7;
    int b = a;
    a = a + 1;
    return b * 2 + a;
}

/* 拷贝来源在之后被改：合并必须作废（老根因，留这里做对照） */
int copy_then_write(int n) {
    int acc = 0;
    int s = 0;
    while (n > 0) {
        int t = acc;
        s = s + t;
        acc = acc + n;
        n--;
    }
    return s + acc;
}

/* 指针写与一次性拷贝混排：被取址的名字不得当稳定来源 */
int copy_of_taken(int n) {
    int acc = 1;
    int *p = &acc;
    int b = acc;
    *p = *p + 5;
    return b + acc;
}

int main(void) {
    if (tile_like(0) != 10)
        return 1;
    if (tile3(0) != 40)
        return 2;
    if (tile_body(0) != 30)
        return 3;
    if (plain_copy(0) != 22)
        return 4;
    if (copy_then_write(3) != 14)
        return 5;
    if (copy_of_taken(0) != 7)
        return 6;
    if (tile_like(9) != 10)
        return 7;
    return 0;
}
