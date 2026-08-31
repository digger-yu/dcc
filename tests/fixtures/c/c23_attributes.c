/* C23 [[unsequenced]] and [[reproducible]] attributes test */
#include <stdio.h>

/* [[unsequenced]]: function has no side effects, result depends only on args */
[[unsequenced]] int pure_add(int a, int b) {
    return a + b;
}

/* [[reproducible]]: result depends only on args, may have side effects through pointers */
[[reproducible]] int reproducible_mul(int a, int b) {
    return a * b;
}

int main(void) {
    int x = pure_add(3, 4);
    int y = reproducible_mul(5, 6);
    
    printf("pure_add(3, 4) = %d\n", x);
    printf("reproducible_mul(5, 6) = %d\n", y);
    
    if (x == 7 && y == 30) {
        printf("PASS\n");
        return 0;
    }
    printf("FAIL\n");
    return 1;
}
