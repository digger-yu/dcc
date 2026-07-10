int puts(const char *s);
int global_var = 42;

int add(int a, int b) { return a + b; }

int main(void) {
    int r = add(global_var, 1);
    if (r == 43) {
        puts("43");
    }
    return r - 43;
}
