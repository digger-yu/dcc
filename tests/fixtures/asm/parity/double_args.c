// 简单双 double 传参测试
double add2(double x, double y) {
    return x + y;
}

int main(void) {
    double r = add2(1.5, 2.5);
    if (r != 4.0) {
        return 1;
    }
    return 0;
}
