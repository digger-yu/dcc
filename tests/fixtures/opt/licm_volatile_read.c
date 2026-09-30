/* LICM 禁止夹具 ⑤：体内 volatile 读。
 *
 * volatile 读本身就是可观测副作用，与"有没有调用"无关：把 `int t = vcounter;`
 * 外提会把每轮采样压成一次，结果从 6 变 0（要求 1 的 volatile 分支）。
 * 全局 volatile 与局部 volatile 各测一条，两条走不同的收集路径
 * （GlobalFacts::volatile_globals vs NameScan::volatiles）。
 */
volatile int vcounter = 0;

int sample(int n) {
    int s = 0;
    while (n > 0) {
        int t = vcounter;
        s += t;
        vcounter = vcounter + 1;
        n--;
    }
    return s;
}

int sample_local(int n) {
    volatile int v = 0;
    int s = 0;
    while (n > 0) {
        int t = v;
        s += t;
        v = v + 2;
        n--;
    }
    return s + v;
}

int main(void) {
    if (sample(4) != 6) return 1;
    if (vcounter != 4) return 2;
    if (sample(0) != 0) return 3;
    if (vcounter != 4) return 4;
    if (sample_local(3) != 12) return 5;
    if (sample_local(1) != 2) return 6;
    if (sample_local(0) != 0) return 7;
    return 0;
}
