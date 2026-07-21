/* 全局数组 RIP-relative + 下标单指令加载 */

int g_arr[4] = {10, 20, 30, 40};
double g_dbl[2] = {1.5, 2.5};

int test_global_int_index(void) {
    int i = 2;
    if (g_arr[i] != 30) return 1;
    if (g_arr[1] != 20) return 2;
    return 0;
}

int test_global_double_index(void) {
    if (g_dbl[0] != 1.5) return 3;
    if (g_dbl[1] != 2.5) return 4;
    return 0;
}

int main(void) {
    int r = test_global_int_index();
    if (r) return 100 + r;
    r = test_global_double_index();
    if (r) return 200 + r;
    return 0;
}
