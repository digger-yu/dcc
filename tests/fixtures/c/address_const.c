// 地址常量求值 (M15)
int arr[4] = {1, 2, 3, 4};

int main(void) {
    int *p = &arr[0];
    int s = p[0] + p[1] + p[2] + p[3];
    return s == 10 ? 0 : 1;
}
