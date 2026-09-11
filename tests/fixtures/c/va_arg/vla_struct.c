/* Test case for va_arg with VLA (variable-length array) struct */
#include <stdarg.h>
#include <stdio.h>

extern void abort(void);
extern void exit(int);

/* Simple struct containing a VLA field (GCC extension) */
struct vla_struct {
    char x[0];  /* Flexible array member - similar behavior to VLA */
};

/* Test with actual VLA in function parameter */
void test_vla_struct(int size, ...) {
    va_list ap;
    va_start(ap, size);
    
    /* First VLA struct argument */
    struct { char data[size]; } first = va_arg(ap, typeof(first));
    
    /* Second VLA struct argument */
    struct { char data[size]; } second = va_arg(ap, typeof(second));
    
    va_end(ap);
    
    /* Verify the data was correctly passed */
    int errors = 0;
    for (int i = 0; i < size; i++) {
        if (first.data[i] != '0' + i) {
            fprintf(stderr, "First struct error at index %d: got %c, expected %c\n", 
                    i, first.data[i], '0' + i);
            errors++;
        }
        if (second.data[i] != '5' + i) {
            fprintf(stderr, "Second struct error at index %d: got %c, expected %c\n", 
                    i, second.data[i], '5' + i);
            errors++;
        }
    }
    
    if (errors > 0) {
        fprintf(stderr, "Total %d errors in va_arg VLA struct test\n", errors);
        abort();
    }
}

/* Another test with mixed types */
int test_mixed_va_arg(int size, ...) {
    va_list ap;
    va_start(ap, size);
    
    int marker = va_arg(ap, int);
    struct { char data[size]; } s1 = va_arg(ap, typeof(s1));
    int another = va_arg(ap, int);
    struct { char data[size]; } s2 = va_arg(ap, typeof(s2));
    
    va_end(ap);
    
    /* Validate marker and another values */
    if (marker != 42 || another != 99) {
        return 1;
    }
    
    /* Validate struct data */
    for (int i = 0; i < size; i++) {
        if (s1.data[i] != 'a' + i || s2.data[i] != 'b' + i) {
            return 1;
        }
    }
    
    return 0;
}

int main(void) {
    int z = 5;
    
    /* Allocate VLA structs on stack */
    struct { char data[z]; } x, y;
    struct { char data[z]; } p, q;
    
    /* Initialize first pair */
    for (int i = 0; i < z; i++) {
        x.data[i] = '0' + i;
        y.data[i] = '5' + i;
    }
    
    /* Initialize second pair */
    for (int i = 0; i < z; i++) {
        p.data[i] = 'a' + i;
        q.data[i] = 'b' + i;
    }
    
    /* Test basic VLA struct passing */
    test_vla_struct(z, x, y);
    
    /* Test mixed va_arg with VLA structs */
    if (test_mixed_va_arg(z, 42, p, 99, q) != 0) {
        fprintf(stderr, "Mixed va_arg test failed\n");
        abort();
    }
    
    exit(0);
}
