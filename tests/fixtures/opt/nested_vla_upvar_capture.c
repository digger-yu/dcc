/* N5/N9 夹具：嵌套函数捕获外层变量与 VLA 运行时尺寸。
 *
 * 三条形状各钉一条通路，缺任何一条通路这里的数就是错的：
 *   (a) `long a[n]` —— 维度 `n` 挂在**声明的类型树**上，不在 init 也不在任何语句里。
 *       旧 `collect_upvars` 只看 `v.init` 与语句 ⇒ `n` 不进 upvars ⇒ 没有静态链槽
 *       ⇒ `sizeof a` 按 0 算（实测修前 `d=14`，gcc 是 `d=72`）。
 *   (b) `sum(int arr[q])` —— 维度在**嵌套函数自己的形参类型**里，同一棵树。
 *   (c) `if (q > 0) { short c[n]; }` —— 语句子树里的声明，走 `Stmt::If` 的递归。
 *
 * 另外钉住 N9（同一轮实测到，与嵌套函数无关）：VLA 元素访问的基址必须从**元数据
 * 槽里加载**（`mov -96(%r13),%rax`），不能 `lea` 那个槽本身。旧形状把读写都落在
 * 描述符区，先踩掉自己的 stride/size 槽，`sizeof` 就返回被写进数组的那个值
 * （实测 `long c[n]; c[2]=7; sizeof c` 得 7 而不是 `8n`）。
 */
#include <stdio.h>

int main(void) {
    int n = 5;
    int q = 3;

    /* (a) VLA 声明维度直接引用外层变量：引用只挂在类型树上 */
    auto long dims(void) {
        long a[n];
        long b[q];
        for (long i = 0; i < n; i++) a[i] = i;
        for (long i = 0; i < q; i++) b[i] = i * 2;
        return (long)(sizeof a) + (long)(sizeof b) + a[n - 1] + b[q - 1];
    }

    /* (b) 嵌套函数的 VLA 形参维度引用外层变量：维度在形参类型里 */
    auto long sum(int arr[q]) {
        long s = 0;
        for (int i = 0; i < q; i++) s += arr[i];
        return s;
    }

    /* (c) 语句嵌套里的声明（if 分支内）同样要走类型树 */
    auto long inside(void) {
        long t = 0;
        if (q > 0) {
            short c[n];
            for (int i = 0; i < n; i++) c[i] = (short)i;
            t = (long)(sizeof c) + c[n - 1];
        }
        return t;
    }

    /* N9: 普通（非嵌套）函数里的 VLA —— 元素读写与 sizeof 必须共用同一个
     * 数据区基址，且读写要真的落在 `sub rsp` 出来的动态区里 */
    long plain(int k) {
        long c[k];
        for (int i = 0; i < k; i++) c[i] = 0;
        c[2] = 7;
        long s = 0;
        for (int i = 0; i < k; i++) s += c[i];
        return (long)(sizeof c) + s;
    }

    long d = dims();
    int arr[3] = { 1, 2, 4 };
    long s = sum(arr);
    long v = inside();
    long p = plain(n);
    printf("d=%ld s=%ld v=%ld p=%ld\n", d, s, v, p);
    return (d == 72 && s == 7 && v == 14 && p == 47) ? 0 : 1;
}
