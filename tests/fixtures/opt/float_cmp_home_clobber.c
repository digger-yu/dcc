/* 浮点比较不得踩碎住在 %rbx 里的寄存器家。
 *
 * 根因（2026-10 修）：`gen_float_cmp` 把 PF 修正硬编码成 `setp %bl` / `setnp %bl`。
 * 而 rbx 会被 regalloc 钉成某个局部的家——下面 `f()` 里的 `len`（snprintf 的返回值）
 * 正好住在 rbx，`check != n` 的 `setp %bl` 把它的低 8 位清零，于是长度从 3 变成 0，
 * `buff[len++] = '.'` 写到 buff[0]，把 "1.5" 覆盖成 ".5"、把 "12" 覆盖成 ".0"。
 *
 * 真实形态是 Lua 的 `luaO_tostringbuff`：dcc -O2 编出的 lua 里
 * `print(12.0)` 打印 ".0"、`print(math.sqrt(144))` 打印 ".0"。
 *
 * 修复要点：`len` 必须是**会被家钉住**的形态（在浮点比较之后仍然存活的 int 局部），
 * 且比较两边都要是运行期值（常量传播会把比较折掉，就碰不到这段代码了）。
 * 判据：四档全部返回 0；家被踩碎时 f() 返回 0 而不是 3/2，main 立刻非 0。
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int f(double n, char *buff) {
    int len = snprintf(buff, 64, "%.14g", (double)n);
    double check = strtod(buff, NULL);
    if (check != n)                      /* 浮点 Ne: 旧代码在这里发 setp %bl */
        len = snprintf(buff, 64, "%.17g", (double)n);
    if (buff[strspn(buff, "-0123456789")] == '\0') {
        buff[len++] = '.';
        buff[len++] = '0';
        buff[len] = '\0';
    }
    return len;                          /* len 住在 %rbx，必须活到这里 */
}

int main(void) {
    char b[64];
    if (f(12.0, b) != 4) return 1;       /* 期望 "12.0"，被踩碎时为 ".0" */
    if (strcmp(b, "12.0") != 0) return 2;
    if (f(1.5, b) != 3) return 3;        /* 期望 "1.5"，被踩碎时 len 报 0 */
    if (strcmp(b, "1.5") != 0) return 4;
    if (f(0.0, b) != 3) return 5;
    if (strcmp(b, "0.0") != 0) return 6;
    return 0;
}
