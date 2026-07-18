// tests/fixtures/attr_cleanup.c
// __attribute__((cleanup(fn))) 在变量离开作用域时调用 fn(&var)。

static void int_cleanup(int *p) {
    *p = 0;
}

static void str_cleanup(char **p) {
    *p = (char *)0;
}

int main(void) {
    {
        int x __attribute__((cleanup(int_cleanup))) = 42;
        if (x != 42) {
            return 1;
        }
    }
    char *s __attribute__((cleanup(str_cleanup))) = (char *)"hello";
    if (s[0] != 'h') {
        return 2;
    }
    return 0;
}
