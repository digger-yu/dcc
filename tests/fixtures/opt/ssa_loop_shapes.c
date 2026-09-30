/* 循环形态的优化档位等价夹具：do-while / 嵌套 for / 数组填充 / continue / break。
 *
 * 这些形态在 SSA 管线里过去被 `func_has_loop` 闸门整体跳过（不进 SSA）。本轮把
 * 循环的 phi 构造改成真正的 SSA（在循环体全域收集使用 + 把体内引用重命名到 phi dest
 * + 记真实 preheader），这些形态就是「闸门能否永久解除」的可度量基线：
 * 闸门关闭（当前默认）时四档全对；用 `DCC_SSA_LOOPS=1` 打开闸门时，本夹具会
 * 精确暴露剩余失效（continue 的回边被发射成空 if 体、嵌套循环计数翻倍、
 * GVN 在循环 IR 上把累加归一并丢失回边）。因此本夹具同时是回归守卫和进度度量。
 *
 * 通过: exit 0
 */
int dw(int n) {
    int s = 0;
    do {
        s += n;
        n--;
    } while (n > 0);
    return s; /* n=3 → 6 */
}

int nest(int n) {
    int s = 0;
    for (int i = 0; i < n; i++)
        for (int j = 0; j < n; j++)
            s += 1;
    return s; /* n=3 → 9 */
}

int fill(int n) {
    int a[4];
    a[0] = 0;
    a[1] = 0;
    a[2] = 0;
    a[3] = 0;
    for (int i = 0; i < n; i++)
        a[i] = i * 2;
    return a[0] + a[1] + a[2] + a[3]; /* n=3 → 0+2+4 = 6 */
}

int cont(int n) {
    int s = 0;
    for (int i = 0; i < n; i++) {
        if (i == 1)
            continue;
        s += i;
    }
    return s; /* n=4 → 0+2+3 = 5 */
}

int brk(int n) {
    int s = 0;
    for (int i = 0; i < n; i++) {
        if (i == 3)
            break;
        s += i;
    }
    return s; /* n=5 → 0+1+2 = 3 */
}

int main(void) {
    if (dw(0) != 0) return 1;
    if (dw(3) != 6) return 2;
    if (nest(0) != 0) return 3;
    if (nest(3) != 9) return 4;
    if (fill(0) != 0) return 5;
    if (fill(3) != 6) return 6;
    if (cont(0) != 0) return 7;
    if (cont(4) != 5) return 8;
    if (brk(0) != 0) return 9;
    if (brk(5) != 3) return 10;
    return 0;
}
