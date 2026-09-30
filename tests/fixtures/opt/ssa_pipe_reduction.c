/* P16 软件流水的执行级证据：被流水的循环必须与 -O0 逐字节同结果。
 *
 * 落地的形式是 peel + kernel + 轮转 phi：
 *   prologue 实例追加到 preheader，kernel 重排成「按绝对时间 T(p,k)=stage[p]+k·II
 *   的行序 + 一级轮转 phi」，epilogue 实例前置到出口块。
 * 正确性全靠两条可判定的自检：调度结果逐边校验（含本 IR 特有的基名塌缩序边与内存
 * 定序边），以及三段展开与 T 序逐元素相等、每个 (节点,迭代) 恰好覆盖一次。
 *
 * 下面每个函数都故意用**编译期常量**的 trip count（流水准入要求），并让期望值
 * 直接由 printf 打印，因此 -O0/-O2/-O3 三方对照即等于「优化没改变行为」。
 *
 * 通过: 与 gcc 各档位输出逐字节一致
 */
#include <stdio.h>

static int arr[12];

/* 最基本的可流水归约：load 延迟 2 + 累加 ⇒ II=3、有一级重叠 */
int red8(void) {
    int s = 0;
    for (int i = 0; i < 8; i++) s += arr[i];
    return s;
}

/* 归约里带乘法（依赖链更长，但仍是同一迭代形状） */
int red8_scaled(void) {
    int s = 0;
    for (int i = 0; i < 8; i++) s += arr[i] * 2;
    return s;
}

/* trip 不是 II 的整数倍：三段展开覆盖的是迭代实例，不要求每轮等长 */
int red7(void) {
    int s = 0;
    for (int i = 0; i < 7; i++) s += arr[i];
    return s;
}

/* 步长 2：trip 向上取整 */
int red_step2(void) {
    int s = 0;
    for (int i = 0; i < 9; i += 2) s += arr[i];
    return s;
}

/* 两个独立累加器（携带依赖不止一条链） */
int red_two_acc(void) {
    int a = 0, b = 0;
    for (int i = 0; i < 8; i++) { a += arr[i]; b -= arr[i]; }
    return a + b * 2;
}

/* 纯写循环：体内没有标量递推，落地不得凭空造出携带依赖 */
int wr_only(void) {
    for (int i = 0; i < 8; i++) arr[i] = i * 3;
    int s = 0;
    for (int i = 0; i < 8; i++) s += arr[i];
    return s;
}

/* trip 太小（闸门要求 trip >= 2*II）：必须原样保留，不流水 */
int red_tiny(void) {
    int s = 0;
    for (int i = 0; i < 2; i++) s += arr[i];
    return s;
}

/* 循环后还要读累加器：出口侧的最后实例必须是它看到的值 */
int red_then_use(void) {
    int s = 0;
    for (int i = 0; i < 8; i++) s += arr[i];
    s += 100;
    return s;
}

/* 体内再带分支（携带值穿过 select）：分支内的累加不能被重排掉 */
int red_cond(void) {
    int s = 0;
    for (int i = 0; i < 8; i++) {
        if (arr[i] & 1) s += arr[i];
        else s += 1;
    }
    return s;
}

int main(void) {
    for (int i = 0; i < 12; i++) arr[i] = i;
    printf("red8=%d\n", red8());
    printf("red8_scaled=%d\n", red8_scaled());
    printf("red7=%d\n", red7());
    printf("red_step2=%d\n", red_step2());
    printf("red_two_acc=%d\n", red_two_acc());
    printf("wr_only=%d\n", wr_only());
    printf("red_tiny=%d\n", red_tiny());
    printf("red_then_use=%d\n", red_then_use());
    printf("red_cond=%d\n", red_cond());
    return 0;
}
