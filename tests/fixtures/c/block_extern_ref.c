/* 块作用域的 `extern` 声明指明的是**外层全局对象**（C11 6.2.2p4 + 6.7p2-3）：
 * 它不创建对象、不占任何存储，只是让这个名字在本块内指向那个外部链接的对象。
 *
 * 缺陷形状：codegen 的规划期把「既不在本帧规划表、也不在本 TU 文件作用域全局表」
 * 的名字一律 `register_var` 分配一个帧内槽，于是
 *   `void bind(void){ extern int shared; p = &shared; }`
 * 交出的是**栈槽地址**。SQLite 测试套的 `Sqlitetest2_Init` 正是用这种声明把六个
 * io-error 全局变量 `Tcl_LinkVar` 给 Tcl：链到一个从不代表那个全局的地址之后，
 * io-error 故障注入整体失效（单跑 altermalloc2 的 ioerr 循环只剩 1 个用例，gcc 是
 * 20；在 quick.test 里同一个循环反而失控到 100 万+ 用例）。
 *
 * 期望（与 gcc 逐字节对照）：same=1 arr_same=1 val=114
 */
#include <stdio.h>

int shared = 100;
int arr[3] = { 1, 2, 3 };

static void *p_shared;
static void *p_arr;

/* 只在这些函数体里声明，不在文件作用域声明 —— 名字必须落到外层全局对象 */
void bind(void) {
    extern int shared;
    extern int arr[];
    p_shared = &shared;
    p_arr = &arr;
}

void bump(void) {
    extern int shared;
    shared += 7;
}

int read_it(void) {
    extern int shared;
    return shared;
}

int main(void) {
    bind();
    bump();
    bump();
    printf("same=%d arr_same=%d val=%d\n",
           p_shared == (void *)&shared, p_arr == (void *)arr, read_it());
    return 0;
}
