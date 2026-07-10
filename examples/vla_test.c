// 可变长数组测试
int main() {
    int n = 5;
    int arr[n];
    
    // 填充数组
    arr[0] = 10;
    arr[1] = 20;
    arr[2] = 30;
    arr[3] = 40;
    arr[4] = 50;
    
    // 求和
    int sum = arr[0] + arr[1] + arr[2] + arr[3] + arr[4];
    
    return sum;  // 应返回 150
}
