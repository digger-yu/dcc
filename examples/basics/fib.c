// examples/fib.c - 检验控制流和函数调用
int fib(int n) {
    if (n < 2) return n;
    return fib(n - 1) + fib(n - 2);
}

int main() {
    int x = fib(10);
    return x;   // 期望 55
}
