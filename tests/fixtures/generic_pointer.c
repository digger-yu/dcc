/* _Generic 指针精确匹配：char* 与 int* 不得互匹配 */
int pick_ptr(void *p) {
    return _Generic(p,
                    char *: 1,
                    int *: 2,
                    default: 3);
}

int main(void) {
    char c = 0;
    int i = 0;
    if (pick_ptr(&c) != 1) return 1;
    if (pick_ptr(&i) != 2) return 2;
    if (pick_ptr((void *)0) != 3) return 3;
    return 0;
}
