/*
 * bootstrap_selftest.c — DCC 自举验证程序
 *
 * 综合测试 dcc 编译器的 C 前端 + 后端 + 优化器能力。
 * 覆盖: 结构体、指针、函数指针、递归、字符串操作、
 *       位运算、枚举、联合体、switch、goto、内联汇编、
 *       变参宏、_Static_assert、typeof、语句表达式等。
 *
 * 返回 0 表示全部自检通过, 非零表示失败。
 */

#include <stdio.h>
#include <stdlib.h>

/* ── 1. 结构体 + 指针 + 函数指针 ── */

typedef struct {
    int x, y;
} Point;

typedef int (*binary_fn)(int, int);

static int my_add(int a, int b) { return a + b; }
static int my_mul(int a, int b) { return a * b; }

static int apply(binary_fn fn, int a, int b) {
    return fn(a, b);
}

/* ── 2. 递归: Fibonacci ── */

static long fib(int n) {
    if (n <= 1) return n;
    return fib(n - 1) + fib(n - 2);
}

/* ── 3. 链表操作 ── */

typedef struct Node {
    int val;
    struct Node *next;
} Node;

static Node *list_push(Node *head, int val) {
    Node *n = (Node *)malloc(sizeof(Node));
    if (!n) return head;
    n->val = val;
    n->next = head;
    return n;
}

static int list_sum(Node *head) {
    int s = 0;
    for (Node *p = head; p; p = p->next)
        s += p->val;
    return s;
}

static void list_free(Node *head) {
    while (head) {
        Node *tmp = head;
        head = head->next;
        free(tmp);
    }
}

/* ── 4. 位运算 ── */

static unsigned int bit_reverse(unsigned int v) {
    unsigned int r = 0;
    int bits = 32;
    while (bits--) {
        r = (r << 1) | (v & 1);
        v >>= 1;
    }
    return r;
}

static int popcount(unsigned int v) {
    int c = 0;
    while (v) {
        c += v & 1;
        v >>= 1;
    }
    return c;
}

/* ── 5. 枚举 + 联合体 ── */

typedef enum { TOK_NUM, TOK_ADD, TOK_SUB, TOK_MUL } TokenKind;

typedef struct {
    TokenKind kind;
    union {
        int num;
        char op;
    } data;
} Token;

static Token make_num(int n) {
    Token t;
    t.kind = TOK_NUM;
    t.data.num = n;
    return t;
}

static Token make_op(TokenKind k) {
    Token t;
    t.kind = k;
    t.data.op = (k == TOK_ADD) ? '+' : (k == TOK_SUB) ? '-' : '*';
    return t;
}

static int eval_token(Token t, int acc) {
    switch (t.kind) {
    case TOK_NUM: return t.data.num;
    case TOK_ADD: return acc + 1;
    case TOK_SUB: return acc - 1;
    case TOK_MUL: return acc * 2;
    default:      return acc;
    }
}

/* ── 6. 字符串只读操作 ── */

static int my_strlen(const char *s) {
    int n = 0;
    while (s[n]) n++;
    return n;
}

static int my_strchr_count(const char *s, char c) {
    int n = 0;
    while (*s) {
        if (*s == c) n++;
        s++;
    }
    return n;
}

static int my_strcmp(const char *a, const char *b) {
    while (*a && *a == *b) { a++; b++; }
    return (unsigned char)*a - (unsigned char)*b;
}

/* ── 7. long long 大数运算 ── */

static int test_longlong(void) {
    long long a = 1LL << 40;
    long long b = a * 3;
    long long c = b / 3;
    if (c != a) return -1;
    if (a <= 0) return -2;
    long long d = a + a;
    if (d != (1LL << 41)) return -3;
    return 0;
}

/* ── 8. 内联汇编 (x86_64) ── */

#if defined(__x86_64__) || defined(__amd64__)
static int asm_add(int a, int b) {
    int result;
    __asm__ __volatile__(
        "addl %%ebx, %%eax\n\t"
        "movl %%eax, %0"
        : "=r"(result)
        : "a"(a), "b"(b)
    );
    return result;
}
#endif

/* ── 9. 变参宏 ── */

#define MAX_OF_2(a, b)       ((a) > (b) ? (a) : (b))
#define MAX_OF_3(a, b, c)    MAX_OF_2(MAX_OF_2(a, b), c)

/* ── 10. _Static_assert + typeof ── */

_Static_assert(sizeof(int) == 4, "int must be 4 bytes");
_Static_assert(sizeof(long) == 8, "long must be 8 bytes on x86_64");

/* ── 11. 复合语句表达式 (GNU 扩展) ── */

#define MAX_STMT_EXPR(a, b) \
    ({ __typeof__(a) _a = (a); __typeof__(b) _b = (b); _a > _b ? _a : _b; })

/* ── 12. goto 前向跳转 ── */

static int goto_test(int n) {
    int sum = 0;
    int i = 0;
loop_start:
    if (i > n) goto loop_end;
    sum += i;
    i++;
    goto loop_start;
loop_end:
    return sum;
}

/* ── 13. switch 范围 (GNU case range) ── */

static int classify_char(char c) {
    switch (c) {
    case '0' ... '9': return 1;
    case 'a' ... 'z': return 2;
    case 'A' ... 'Z': return 3;
    default:           return 0;
    }
}

/* ── 14. 静态数组 + 初始化器 ── */

static const int primes[] = {2, 3, 5, 7, 11, 13, 17, 19, 23, 29};

static int sum_primes(void) {
    int s = 0;
    int n = (int)(sizeof(primes) / sizeof(primes[0]));
    for (int i = 0; i < n; i++)
        s += primes[i];
    return s;
}

/* ── 主测试入口 ── */

int main(void) {
    int fail = 0;

    /* 1. 结构体 + 函数指针 */
    Point p = {3, 4};
    if (apply(my_add, p.x, p.y) != 7) { printf("FAIL: func_ptr add\n"); fail++; }
    if (apply(my_mul, p.x, p.y) != 12) { printf("FAIL: func_ptr mul\n"); fail++; }

    /* 2. Fibonacci */
    if (fib(10) != 55)  { printf("FAIL: fib(10)=%ld\n", fib(10)); fail++; }
    if (fib(20) != 6765) { printf("FAIL: fib(20)=%ld\n", fib(20)); fail++; }

    /* 3. 链表 */
    Node *list = NULL;
    for (int i = 1; i <= 10; i++)
        list = list_push(list, i);
    if (list_sum(list) != 55) { printf("FAIL: list_sum\n"); fail++; }
    list_free(list);

    /* 4. 位运算 */
    if (popcount(0xFF) != 8) { printf("FAIL: popcount\n"); fail++; }
    if (bit_reverse(1) != 0x80000000u) { printf("FAIL: bit_reverse\n"); fail++; }

    /* 5. 枚举 + 联合体 */
    Token tn = make_num(42);
    Token ta = make_op(TOK_ADD);
    if (eval_token(tn, 0) != 42) { printf("FAIL: eval num\n"); fail++; }
    if (eval_token(ta, 10) != 11) { printf("FAIL: eval add\n"); fail++; }

    /* 6. 字符串只读操作 */
    {
        const char *hello = "hello world";
        if (my_strchr_count(hello, 'l') != 3) { printf("FAIL: count chars\n"); fail++; }
        if (my_strlen(hello) != 11) { printf("FAIL: strlen=%d\n", my_strlen(hello)); fail++; }
        if (my_strcmp("abc", "abc") != 0) { printf("FAIL: strcmp eq\n"); fail++; }
        if (my_strcmp("abc", "abd") >= 0) { printf("FAIL: strcmp lt\n"); fail++; }
    }

    /* 7. long long 大数 */
    if (test_longlong() != 0) { printf("FAIL: longlong (code %d)\n", test_longlong()); fail++; }

    /* 8. 内联汇编 */
#if defined(__x86_64__) || defined(__amd64__)
    if (asm_add(100, 200) != 300) { printf("FAIL: asm_add\n"); fail++; }
#endif

    /* 9. 变参宏 */
    if (MAX_OF_3(10, 20, 15) != 20) { printf("FAIL: max_of_3\n"); fail++; }

    /* 10. _Static_assert — 编译期验证, 通过即成功 */

    /* 11. 语句表达式 */
    if (MAX_STMT_EXPR(42, 17) != 42) { printf("FAIL: stmt_expr\n"); fail++; }

    /* 12. goto */
    if (goto_test(100) != 5050) { printf("FAIL: goto_test=%d\n", goto_test(100)); fail++; }

    /* 13. switch range */
    if (classify_char('5') != 1) { printf("FAIL: classify digit\n"); fail++; }
    if (classify_char('m') != 2) { printf("FAIL: classify lower\n"); fail++; }
    if (classify_char('Z') != 3) { printf("FAIL: classify upper\n"); fail++; }
    if (classify_char('!') != 0) { printf("FAIL: classify other\n"); fail++; }

    /* 14. 静态数组 */
    if (sum_primes() != 129) { printf("FAIL: sum_primes=%d\n", sum_primes()); fail++; }

    /* 结果汇总 */
    if (fail == 0) {
        printf("bootstrap selftest: ALL 14 TEST GROUPS PASSED\n");
        return 0;
    } else {
        printf("bootstrap selftest: %d FAILURES\n", fail);
        return 1;
    }
}
