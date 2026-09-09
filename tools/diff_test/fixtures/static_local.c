/* static local variable storage model test */

static int global_x;

/* Different functions can have same-named static locals */
int fa(void) { static int x; return ++x; }
int fb(void) { static int x; return ++x; }

/* Static local array with constant initializer → .data */
void fc(void) {
    static int arr[4] = {1, 2, 3, 4};
    (void)arr;
}

/* Static local string-initialized array → .data */
void fd(void) {
    static char s[8] = "hello";
    (void)s;
}

/* Static local uninitialized array → .bss */
void fe(void) {
    static int uninit[10];
    (void)uninit;
}

/* Static local scalar with initializer → .data */
int ff(void) {
    static int x = 42;
    return x;
}

/* Static local in nested block */
int fg(int x) {
    if (x > 0) {
        static int n;
        n += x;
        return n;
    }
    return 0;
}

int main(void) {
    fa(); fb();
    return fa() - fb();
}
