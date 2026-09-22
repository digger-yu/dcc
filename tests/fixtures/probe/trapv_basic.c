/* -ftrapv 探针: 有符号加法溢出需落到 trap 辅助函数 */
int tv_add(int a, int b) { return a + b; }
int tv_sub(int a, int b) { return a - b; }
