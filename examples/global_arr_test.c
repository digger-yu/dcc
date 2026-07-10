// 测试全局数组初始化
int global_arr[3] = {1, 2, 3};

int main() {
    if (global_arr[0] != 1) return 1;
    if (global_arr[1] != 2) return 2;
    if (global_arr[2] != 3) return 3;
    return 0;
}
