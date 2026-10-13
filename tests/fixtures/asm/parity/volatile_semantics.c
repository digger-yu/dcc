/* Test: C89 volatile semantics preservation */
/* volatile variables should not be optimized away */

volatile int flag = 0;
volatile int sensor_data;

/* volatile read should not be eliminated */
int read_flag(void) {
    return flag;
}

/* volatile write should not be eliminated */
void set_flag(int v) {
    flag = v;
}

/* volatile local variable */
int test_local_volatile(void) {
    volatile int x = 42;
    return x;  /* read of volatile must occur */
}

/* volatile in loop - must re-read each iteration */
void wait_for_flag(void) {
    while (flag == 0) {
        /* spin - volatile prevents loop optimization */
    }
}

/* mixed volatile and non-volatile */
int mixed_access(void) {
    int normal = 10;
    volatile int vol = 20;
    return normal + vol;
}

int main(void) {
    sensor_data = 100;
    set_flag(1);
    wait_for_flag();
    return test_local_volatile() + mixed_access();
}
