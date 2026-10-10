// 指针运算缩放测试
// int=4字节, ptr+1 应步进4; ptr-ptr 应返回元素数
// 注意: 数组必须声明在最后 (栈帧布局限制: 数组向高地址增长)
int main(void) {
    int *p;
    int *q;
    int v;
    int diff;
    int v2;
    int arr[5];
    arr[0] = 10; arr[1] = 20; arr[2] = 30; arr[3] = 40; arr[4] = 50;
    p = arr;
    // 指针+整数: p+3 应跳过 3*4=12 字节
    v = *(p + 3);
    // 指针-指针: 两个指针差 / 4 = 元素数
    q = p + 4;
    diff = q - p;
    // 指针自增: p++ 步进 4 字节
    p++;
    v2 = *p;
    // 返回 v + diff + v2 = 40 + 4 + 20 = 64
    return v + diff + v2;
}
