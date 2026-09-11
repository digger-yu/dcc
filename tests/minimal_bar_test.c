/* Minimal test for static local post-increment */
#include <stdio.h>
#include <stdlib.h>

extern void abort(void);

void bar(int c) {
    static int d = '0';
    
    if (c != d++) {
        fprintf(stderr, "FAIL: c=%d (%c), d++ old value=%d (%c)\n", 
                c, c, c - ('0' - '0'), '0');
        abort();
    }
    if (c < '0' || c > '9') {
        fprintf(stderr, "FAIL: c out of range\n");
        abort();
    }
}

int main(void) {
    /* Test sequence: '0', '1', '2', '3', '4' with static d */
    bar('0');  /* d starts as '0', d++ returns '0', then d becomes '1' */
    bar('1');  /* d is '1', d++ returns '1', then d becomes '2' */
    bar('2');  
    bar('3');  
    bar('4');  
    
    /* Continue with more values */
    bar('5');
    bar('6');
    bar('7');
    bar('8');
    bar('9');
    
    printf("SUCCESS: All tests passed!\n");
    exit(0);
}
