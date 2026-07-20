// 地址常量求值 (M15)
extern int printf(const char *, ...);

int arr[4] = {1, 2, 3, 4};

int main(void) {
    int *p = &arr[0];
    printf("%d %d %d %d\n", p[0], p[1], p[2], p[3]);
    return 0;
}
