/* M1.2: multi-file compilation - main module */
#include <stdio.h>

extern int global_counter;
extern int m1_2_add(int a, int b);
extern void m1_2_increment(void);
extern int m1_2_get_counter(void);

static int local_val = 100;

static inline int square(int x) {
    return x * x;
}

int main(void) {
    int sum = m1_2_add(3, 4);
    int sq = square(5);
    m1_2_increment();
    m1_2_increment();
    m1_2_increment();
    /* encode result in exit code: sum(7)*1000 + sq(25)*10 + counter(3) = 7253 */
    /* but exit code is 0-255, so use printf and check stdout */
    printf("sum=%d sq=%d ctr=%d loc=%d\n", sum, sq, m1_2_get_counter(), local_val);
    return 0;
}
