// __COUNTER__ 在宏体中也能递增 (M19)
#define MAKE_VAR(name) int name = __COUNTER__
int main(void) {
    MAKE_VAR(a);
    MAKE_VAR(b);
    MAKE_VAR(c);
    if (a != 0) return 1;
    if (b != 1) return 2;
    if (c != 2) return 3;
    return 0;
}
