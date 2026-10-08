/* N7 实测夹具：用户 asm 文本里 `call` 一个 file-scope static 函数。
 *
 * 旧记录（§11.13 的 N7）说「-O2 下符号被改名 ⇒ 链接失败」。本轮按 gcc 复测的结论
 * 是**两条**，都不是改名：
 *   1) C 侧**不**引用 `leaf` 时，gcc -O2 与 dcc -O2 一样把它的定义整个丢掉
 *      （`nm` 两侧都是 `U leaf`，两侧链接都报同一个 `undefined reference to 'leaf'`）
 *      —— 这是 gcc 自己的既有限制（asm 文本不参与符号引用分析），不是 dcc 的偏差。
 *   2) C 侧也引用一次（本夹具）时，符号必须保持**源名** `leaf`、必须真的定义出来、
 *      并且可链接可运行。dcc -O0/-O2 都是 `t leaf` + 退出码 5；gcc -O2 在这一档
 *      自己就链不上，所以 oracle 只能取 gcc -O0。
 *
 * `probe` 里那次 `call leaf` 依赖被调函数按 SysV 约定读写 %rdi/%rax；调用点两侧
 * 各加一次 C 侧调用，保证「asm 引用的那个符号」与「C 引用的那个符号」是同一个。
 */
static int leaf(int x) { return x + 1; }

int probe(int v) {
    __asm__ volatile("call leaf" : "+r"(v));
    return leaf(v);
}

int main(void) {
    return probe(3); /* 3 -> leaf(call) 4 -> C 侧 leaf 5 */
}
