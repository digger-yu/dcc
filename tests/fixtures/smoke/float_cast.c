// 测试浮点类型转换
int main() {
    // int → double
    int a = 42;
    double b = (double)a;  // 42.0
    
    // double → int
    double c = 3.14;
    int d = (int)c;  // 3 (截断)
    
    // float → double
    float e = 1.5f;
    double f = (double)e;  // 1.5
    
    // double → float
    double g = 2.5;
    float h = (float)g;  // 2.5
    
    // 验证结果
    int sum = d + (int)f + (int)h;  // 3 + 1 + 2 = 6
    
    return 0;  // 0 = 成功
}
