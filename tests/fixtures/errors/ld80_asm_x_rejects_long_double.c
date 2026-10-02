/* a6 重点1：long double 经 SSE (`x`) 约束是**非法**的，必须诊断拒绝。
 *
 * SysV 上 long double / complex long double 是 x87 专有类型，gcc 对
 * `"=x"(long double)` 报：
 *     error: inconsistent operand constraints in an 'asm'
 * 并以非 0 退出。dcc 此前默默产出垃圾值（极大乱数）。
 *
 * 四个形态都必须报 E0311 且编译失败：
 *   全局 long double 输入、局部 long double 输入、写回全局 long double、
 *   写回局部 long double。
 *
 * 注意本夹具**期望编译失败**（不应产出可执行文件），由
 * ld80_asm_x_rejects_long_double 测试断言：dcc 退出非 0、stderr 至少
 * 4 条 E0311，且无 panic。
 */
#include <stdio.h>

long double gv = 0.1L;
long double gout = 0.0L;

int main(void) {
    long double lv = 0.1L;
    long double lout = 0.0L;

    long double x1 = 0.0L;
    __asm__("" : "=x"(x1) : "0"(gv));
    printf("x-tglobal %.27Lf\n", x1);

    long double x2 = 0.0L;
    __asm__("" : "=x"(x2) : "0"(lv));
    printf("x-tlocal %.27Lf\n", x2);

    __asm__("" : "=x"(gout) : "0"(gv));
    printf("gout %.27Lf\n", gout);

    __asm__("" : "=x"(lout) : "0"(lv));
    printf("lout %.27Lf\n", lout);
    return 0;
}
