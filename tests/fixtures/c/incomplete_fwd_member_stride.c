/* 前向声明的 struct/union 在**之后**才定义时, 引用它的位置早已被冻结进
 * 类型副本 (struct 成员的 Field.ty、声明早于定义的指针数组)。类型副本里
 * 的占位必须在使用点回填成完整定义, 否则元素大小为 0, 下标不乘步长,
 * `p[i]` 全部退化成 `p[0]`。SQLite 的 `struct VdbeFrame { Mem *aMem; }`
 * (Mem 定义在其后) 触发器 NEW./OLD. 读错内存单元即此形。
 *
 * 约定: 全部检查通过时 main 返回 0。 */

typedef struct Late Late; /* 此刻不完整 */

/* 成员指针: Field.ty 冻结成 Ptr(不完整 Late) */
typedef struct Holder {
  Late *arr;
  int n;
} Holder;

/* 声明早于定义的指针数组 (指针本身不需要完整类型) */
Late *g_tbl[2];

/* 自引用成员: 字段类型在 struct 本体完成之前写下 */
struct Node {
  struct Node *next;
  long long v;
};

struct Late {
  double r;
  long long i;
  char *z;
  int n;
  unsigned short flags;
};

/* 定义之后才有的对象 (C 不允许在不完整期定义) */
Late g_store[4];

#define CHECK(cond) do { if (!(cond)) return __LINE__; } while (0)

int main(void) {
  Holder h;
  h.arr = g_store;
  h.n = 4;

  /* 1. 成员指针下标必须按 sizeof(Late) 缩放 */
  h.arr[0].i = 11;
  h.arr[1].i = 22;
  h.arr[2].i = 33;
  CHECK(g_store[1].i == 22);
  CHECK(g_store[2].i == 33);

  /* 2. sizeof 穿透成员指针的指涉类型 */
  CHECK(sizeof(*h.arr) == sizeof(Late));

  /* 3. 指针加法与下标一致 */
  CHECK(h.arr + 2 == &h.arr[2]);
  CHECK((char *)&h.arr[3] - (char *)h.arr == 3 * (long)sizeof(Late));

  /* 4. 非零下标上的字段读写落在同一个元素 */
  h.arr[2].flags = 7;
  CHECK(g_store[2].flags == 7);
  CHECK(g_store[1].flags == 0);

  /* 5. 指针数组 (声明早于定义) 的两级下标 */
  g_tbl[0] = g_store;
  g_tbl[0][1].n = 99;
  CHECK(g_store[1].n == 99);
  CHECK((char *)&g_tbl[0][3] - (char *)g_tbl[0] == 3 * (long)sizeof(Late));

  /* 6. 自引用成员的下标 */
  {
    struct Node nn[3];
    nn[0].next = nn;
    nn[2].v = 77;
    CHECK(nn[0].next[2].v == 77);
    CHECK((char *)&nn[0].next[1] - (char *)nn[0].next ==
          (long)sizeof(struct Node));
  }

  /* 7. typedef 到前向声明后再定义 (SQLite Mem 的形状) */
  {
    Holder hh;
    Late buf[3];
    hh.arr = buf;
    hh.arr[2].r = 1.5;
    CHECK(buf[2].r == 1.5);
    CHECK(buf[0].r != 1.5);
  }
  return 0;
}
