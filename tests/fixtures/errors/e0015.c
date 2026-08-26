/* E0015: flexible array member in a struct with no named members
 * (C99 6.7.2.1) */
struct s { int a[]; };
int main(void) { return 0; }
