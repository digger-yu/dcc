/* Ternary with side effects: local variables must shadow globals
 * in post-increment/decrement codegen (gen_inc_dec).
 *
 * Root cause: gen_inc_dec checked globals.contains_key() without
 * is_local_name() guard, so local x/y were treated as globals
 * when same-named globals existed.
 *
 * GCC torture test: 20000715-1.c
 */
void abort(void);
void exit(int);

int x, y;

static void init_xy(void) { x = 3; y = 2; }

/* test1: local x,y shadow globals; ternary result checked inline */
void test1(void) {
    int x = 3, y = 2;
    if ((x < y ? x++ : y++) != 2) abort();
    if (x != 3) abort();
    if (y != 3) abort();
}

/* test2: local x,y; ternary result assigned to z */
void test2(void) {
    int x = 3, y = 2, z;
    z = (x < y) ? x++ : y++;
    if (z != 2) abort();
    if (x != 3) abort();
    if (y != 3) abort();
}

/* test3: separate condition vars (xx,yy) from side-effect vars (x,y) */
void test3(void) {
    int x = 3, y = 2;
    int xx = 3, yy = 2;
    if ((xx < yy ? x++ : y++) != 2) abort();
    if (x != 3) abort();
    if (y != 3) abort();
}

/* test4: globals via init_xy() */
void test4(void) {
    init_xy();
    if ((x < y ? x++ : y++) != 2) abort();
    if (x != 3) abort();
    if (y != 3) abort();
}

/* test5: globals assigned to z */
void test5(void) {
    int z;
    init_xy();
    z = (x < y) ? x++ : y++;
    if (z != 2) abort();
    if (x != 3) abort();
    if (y != 3) abort();
}

/* test6: mixed local condition + global side-effect */
void test6(void) {
    int xx = 3, yy = 2;
    int z;
    init_xy();
    z = (xx < y) ? x++ : y++;
    if (z != 2) abort();
    if (x != 3) abort();
    if (y != 3) abort();
}

int main(void) {
    test1();
    test2();
    test3();
    test4();
    test5();
    test6();
    exit(0);
}
