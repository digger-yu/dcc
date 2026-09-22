/* UBSan 探针: 有符号溢出 + 除零, 二者都需要 __ubsan_handle_* 或显式 trap */
int ub_div(int a, int b) { return a / b; }
int ub_add(int a, int b) { return a + b; }
