// 浮点常量求值 (M15)

int main(void) {
    double pi = 3.14159;
    double e = 2.71828;
    double sum = pi + e;
    // sum = 5.85987 (expected within 1e-4 of 5.85987)
    int scaled = (int)(sum * 100000.0);
    return scaled == 585987 ? 0 : 1;
}
