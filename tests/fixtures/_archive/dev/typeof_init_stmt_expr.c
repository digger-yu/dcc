typedef struct { int counter; } atomic_t;

int main(void) {
    atomic_t *v;
    int **old;
    return ({
        __typeof__(((&v->counter))) _old = (__typeof__(((&v->counter))))(((old)));
        0;
    });
}
