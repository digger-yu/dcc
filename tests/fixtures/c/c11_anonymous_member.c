/* Test: C11 anonymous struct/union members */
/* Used by kernel struct_group() macro */

struct outer {
    int a;
    struct {
        int b;
        int c;
    };  /* anonymous struct */
    union {
        long d;
        float e;
    };  /* anonymous union */
    int f;
};

int main(void) {
    struct outer o;
    o.a = 1;
    o.b = 2;  /* access through anonymous struct */
    o.c = 3;  /* access through anonymous struct */
    o.d = 4;  /* access through anonymous union */
    o.f = 5;
    return o.a + o.b + o.c + o.f;
}
