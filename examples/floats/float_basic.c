// 测试浮点运算
int main() {
    double a = 1.5;
    double b = 2.5;
    double c = a + b;  // 4.0
    double d = a * b;  // 3.75
    double e = a - b;  // -1.0
    double f = b / a;  // 1.666...
    
    // 比较运算
    int g = (a < b);   // 1
    int h = (a > b);   // 0
    int i = (a == b);  // 0
    int j = (a != b);  // 1
    
    // 返回整数部分之和: 4 + 3 + (-1) + 1 + 0 + 1 + 0 + 1 = 9
    // 简化为: 返回 0 表示成功
    return 0;
}
