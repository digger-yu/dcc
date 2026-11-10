/* C11 6.7.9p14：字符数组可以由字符串字面量初始化，字面量可以带一层花括号。
 *
 * `static char a[] = { "hello" };` 的维度必须是 6（含 NUL）。按「一项一个元素」
 * 数会把数组定成 1 字节：初始化器实际写 6 字节，踩掉后面的静态变量；
 * `sizeof(a) - 1` 也跟着退化成 0 —— 内核 `fs/coredump.c` 的
 * `static char core_modes[] = { "file\npipe" "\nsocket" };` 就是因此被
 * `sysctl` 判成 `core_modes No maxlen` 而整张表注册失败。 */
static char a[] = { "hello" };
static char b[] = { 'w', 'o', 'r', 'l', 'd' };
static char tail[] = { "ZZ" };

struct ctl {
    const char *procname;
    void *data;
    int maxlen;
};

static const struct ctl tab[] = {
    { .procname = "a", .data = a, .maxlen = sizeof(a) - 1 },
    { .procname = "b", .data = b, .maxlen = sizeof(b) - 1 },
    { .procname = "tail", .data = tail, .maxlen = sizeof(tail) - 1 },
};

int main(void) {
    if (sizeof(a) != 6) return 1;
    if (a[4] != 'o' || a[5] != 0) return 2;
    if (sizeof(b) != 5 || b[4] != 'd') return 3;
    if (sizeof(tail) != 3 || tail[1] != 'Z') return 4;
    if (tab[0].maxlen != 5 || tab[1].maxlen != 4 || tab[2].maxlen != 2) return 5;
    return 0;
}
