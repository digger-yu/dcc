/* C11 _Atomic non-basic types test */
#include <stdio.h>

struct Counter {
    int value;
    int flags;
};

int main(void) {
    /* _Atomic with basic types */
    _Atomic int x = 0;
    x = 42;
    int y = x;
    
    /* _Atomic with struct type */
    _Atomic struct Counter c;
    c.value = 10;
    c.flags = 1;
    
    int val = c.value;
    int flg = c.flags;
    
    printf("x=%d y=%d\n", y, y);
    printf("c.value=%d c.flags=%d\n", val, flg);
    
    if (y == 42 && val == 10 && flg == 1) {
        printf("PASS\n");
        return 0;
    }
    printf("FAIL\n");
    return 1;
}
