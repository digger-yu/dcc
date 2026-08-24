/* Test: stack protector guard register (-mstack-protector-guard-reg=gs) */

int func_with_locals(void) {
    int a = 1;
    int b = 2;
    int c = 3;
    return a + b + c;
}

int func_with_array(void) {
    int arr[16];
    for (int i = 0; i < 16; i++) {
        arr[i] = i;
    }
    return arr[15];
}
