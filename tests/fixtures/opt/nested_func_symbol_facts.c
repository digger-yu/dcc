/* N1 夹具：嵌套函数的**程序级符号事实**必须在并行发射下也带回主 Codegen。
 *
 * 四条形状各钉一件事，少一条就漏一种回归：
 *   1) 两个顶层函数各带一个嵌套函数 —— 唯一名的全局计数必须在并行与串行之间
 *      一致（旧形状在 `compile_func` 里递增计数，每个 worker 都从同一份快照
 *      起号 ⇒ 并行得到 `.0`、串行得到 `.1`）；
 *   2) 同一父函数里两个兄弟嵌套函数 —— 计数必须逐个往前走；
 *   3) 两层嵌套（`leaf` 在 `mid` 里，且引用**祖父帧**的 `base`）—— 符号发射
 *      不能只按顶层父函数查重命名表（旧的那条 per-decl 路径查不到孙层）；
 *   4) `&nb` 取地址 —— trampoline 那条引用同样依赖这批事实。
 *
 * 旧实测（修前，同一份源）：`-O2` 的 .o 里六个嵌套函数符号全是 `U`，链接报
 * `undefined reference to 'ia_add_a.0'`；`DCC_NO_PARALLEL=1` 立即恢复正常。
 * 夹具放在 `opt/` 而不是 `c/`：后者会被三档产物哈希按默认 flags 扫，拿不到
 * 「并行 vs 串行」这条轴。
 */
#include <stdio.h>

static int add_a(int x) {
    int base = 10;
    auto int ia(int y) { return y + base; }
    return ia(x);
}

static int add_b(int x) {
    int base = 20;
    auto int ib(int y) { return y + base; }
    auto int ic(int y) { return y + base * 2; }
    return ib(x) + ic(x);
}

static int deep(int x) {
    int base = 5;
    auto int mid(int y) {
        auto int leaf(int z) { return z + base; }
        return leaf(y) * 2;
    }
    return mid(x);
}

static int use_addr(int x) {
    int base = 3;
    auto int nb(int y) { return y + base; }
    int (*fp)(int) = &nb;
    return fp(x);
}

int main(void) {
    int fail = 0;
    if (add_a(1) != 11) fail |= 1;
    if (add_b(1) != 62) fail |= 2;
    if (deep(2) != 14) fail |= 4;
    if (use_addr(4) != 7) fail |= 8;
    printf("fail=%d\n", fail);
    return fail;
}
