// LTO test: file 2 - main function
extern int add(int a, int b);
extern int multiply(int a, int b);

int main() {
    int sum = add(3, 4);
    int prod = multiply(5, 6);
    return sum + prod;
}
