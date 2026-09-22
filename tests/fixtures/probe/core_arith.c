int add(int a, int b) { return a + b; }
int sub(int a, int b) { return a - b; }
int mul(int a, int b) { return a * b; }
int divmod(int a, int b) { return a / b + a % b; }
int shifts(int a, int b) { return (a << b) | (a >> (b | 1)) ^ (a & b) | (a & ~b); }
int cmp(int a, int b) { return (a < b) + (a <= b) + (a == b) + (a != b); }
