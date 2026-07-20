// 数组下标运行时边界检查 (-fbounds-check)
extern int printf(const char *, ...);

int main(void) {
    int arr[5] = {10, 20, 30, 40, 50};
    int sum = 0;
    for (int i = 0; i < 5; i++) {
        sum += arr[i];
    }
    printf("%d\n", sum);
    return 0;
}
