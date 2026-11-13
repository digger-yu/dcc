/* VM struct（GCC 扩展：含 VLA 字段的记录类型）+ 连续位域的布局自检。
 *
 * `struct S { char d[n]; int a:3, b:17, c:12; }` 里 a/b/c 共享同一个 4 字节
 * 存储单元，动态偏移只能算到单元起点（不是每个位域各推进一个 int）。
 * 同时覆盖 `struct T { char d[n]; int x; }` 的普通（非位域）字段与
 * `struct S[2][3]` 的二维数组 stride —— 元素大小是运行期值。
 *
 * 用返回码汇报，不依赖 printf；main 把第一个失败的点带出来。
 */
static int probe(int n, unsigned long exp_size)
{
  struct S { char d[n]; int a : 3; int b : 17; int c : 12; };
  struct S A[2];
  A[0].a = 1;
  A[0].b = 2;
  A[0].c = 3;
  A[1].a = 4;
  A[1].b = 5;
  A[1].c = 6;
  if (sizeof(struct S) != exp_size)
    return 1;
  if (A[0].a != 1)
    return 2;
  if (A[0].b != 2)
    return 3;
  if (A[0].c != 3)
    return 4;
  if (A[1].a != -4) /* int a:3 有符号: 4 存进 3 位是 -4 */
    return 5;
  if (A[1].b != 5)
    return 6;
  if (A[1].c != 6)
    return 7;
  /* 首个 VLA 字段本身（静态前缀，编译期偏移 0） */
  A[1].d[0] = 'x';
  if (A[1].d[0] != 'x')
    return 8;

  /* 二维数组：stride 错时 B[1][2] 会盖掉 B[0][0] */
  struct S B[2][3];
  B[0][0].b = 1;
  B[1][2].b = 7;
  if (B[0][0].b != 1)
    return 9;
  if (B[1][2].b != 7)
    return 10;

  /* 非位域字段：`char d[n]; int x;` 的 x 在运行期偏移 align_up(n, 4) */
  struct T { char d[n]; int x; };
  struct T TA[3];
  TA[0].x = 1;
  TA[2].x = 9;
  if (TA[0].x != 1)
    return 11;
  if (TA[2].x != 9)
    return 12;
  if (sizeof(struct T) != exp_size)
    return 13;
  return 0;
}

int main(void)
{
  int rc = probe(1, 8);
  if (rc)
    return rc;
  rc = probe(8, 12);
  if (rc)
    return 20 + rc;
  return 0;
}
