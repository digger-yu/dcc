/* 回归夹具: 函数局部 `static` 指针变量的字符串初始化器必须发射重定位。
**
** `register_static_global` (`src/codegen/x86_64/elf/static_locals.rs`) 对
** `Expr::String` + `Ptr` 类型只 `resize` 出 sz 个**零字节**, 不发指向 .rodata 的
** R_X86_64_64 重定位 ⇒ 函数局部 static 指针恒为 NULL (各级别均复现)。
** 文件作用域全局走的是另一条正确路径 (`char *s = "hello"` 分支), 本夹具同时钉住
** 两侧, 防止回归只修一半。
**
** 实测 sqlite `shell.c` 的 `zAutoColumn`: `zTabFill` 等 static const char* 为 NULL,
** `.import` 自动列名路径上 `sqlite3_prepare_v2(db, NULL, ...)` 返回 SQLITE_MISUSE,
** `rc_err_oom_die` 断言中止 (gcc 构建正常)。 */
static int n_hit;

/* 文件作用域: 这条路径本来就是对的, 钉住别回归。 */
static const char *g_ptr = "global-ptr";
static const char *const g_cptr = "global-const-ptr";
static const char g_arr[] = "global-arr";

__attribute__((noinline)) static int use_local(int mode) {
    /* 函数局部 static 指针 (犯案形状, 含 const 组合)。 */
    static const char *const z_make = "CREATE TABLE t(a)";
    static const char *const z_fill = "INSERT INTO t VALUES(iif(length(?1)>0,?1,'?'))";
    static const char *z_writable = "writable-static";
    const char *p;

    if (mode == 0)
        p = z_make;
    else if (mode == 1)
        p = z_fill;
    else
        p = z_writable;
    /* 内容必须真的是那个字符串: 位置对不上就是指针错了。 */
    if (p[0] != 'C' && p[0] != 'I' && p[0] != 'w')
        return -1;
    if (mode == 0 && p[7] != 'T')
        return -2;
    if (mode == 1 && p[8] != 'N')
        return -3;
    n_hit++;
    return 0;
}

int main(void) {
    int mask = 0;

    if (use_local(0) != 0)
        mask |= 1;
    if (use_local(1) != 0)
        mask |= 2;
    if (use_local(2) != 0)
        mask |= 4;
    if (g_ptr[0] != 'g' || g_ptr[7] != 'p')
        mask |= 8;
    if (g_cptr[0] != 'g' || g_cptr[13] != 'p')
        mask |= 16;
    if (g_arr[0] != 'g' || g_arr[9] != 'r')
        mask |= 32;
    return mask;
}
