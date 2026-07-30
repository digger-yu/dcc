/* M1.2: multi-file compilation - helper module */
int global_counter = 0;

int m1_2_add(int a, int b) {
    return a + b;
}

static int multiply(int a, int b) {
    return a * b;
}

void m1_2_increment(void) {
    global_counter = m1_2_add(global_counter, 1);
}

int m1_2_get_counter(void) {
    return multiply(global_counter, 1);
}
