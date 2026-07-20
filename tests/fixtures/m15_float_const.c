// 浮点常量求值 (M15)
extern int printf(const char *, ...);

int main(void) {
    double pi = 3.14159;
    double e = 2.71828;
    double sum = pi + e;
    printf("%.5f\n", sum);
    return 0;
}
