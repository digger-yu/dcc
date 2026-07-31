// __COUNTER__ 单调递增 (M19)
int main(void) {
    int a = __COUNTER__;
    int b = __COUNTER__;
    int c = __COUNTER__;
    if (a != 0) return 1;
    if (b != 1) return 2;
    if (c != 2) return 3;
    return 0;
}
