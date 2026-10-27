/* tests/fixtures/c/deref_array_elem_ty.c
 *
 * 一元 `*` 作用在数组上时，结果类型必须是**元素类型**（C11 6.5.3.2p2 配 6.3.2.1p3：
 * 数组先退化再解引用，`*arr` 就是首元素）。旧实现只对 `Type::Ptr` 算这条类型，其余
 * 一律退到 `int` —— 于是 `sizeof(*arr)` 对函数指针数组算成 4、对字符数组也算成 4。
 * gcc-torture 的 `bcp-1.c` 用 `N(arr) = sizeof(arr)/sizeof(*arr)` 数元素，比例一错就
 * 跑到数组外。
 */
int b0(void) { return 1; }
int b1(void) { return 2; }

static int (*fparr[])(void) = { b0, b1, 0 };

typedef int FP(void);
static FP *fparr2[] = { b0, b1, 0 };

static char msg[] = "hello";

long f(void) {
    /* 元素是函数指针：sizeof(*fparr) 必须等于指针宽度，不是 int 的 4 */
    if (sizeof(*fparr) != sizeof(void *)) {
        return 1;
    }
    /* typedef 写法（`FP *`）同形同判据 */
    if (sizeof(*fparr2) != sizeof(void *)) {
        return 2;
    }
    /* 字符数组：元素是 char（1），旧实现同样退到 int */
    if (sizeof(*msg) != 1) {
        return 3;
    }
    /* 按 sizeof 比例遍历数组本身也必须落在界内 */
    long acc = 0;
    for (unsigned i = 0; i < sizeof fparr / sizeof *fparr; i++) {
        if (fparr[i]) {
            acc += fparr[i]();
        }
    }
    return acc == 3 ? 0 : 4;
}

int main(void) { return f(); }
