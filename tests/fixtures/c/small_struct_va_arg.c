/* 小尺寸聚合（3/5 字节）的变参交付与空结构体返回。
 *
 * 1) va_arg(struct{char c,d,e;}) 与 5 字节变体：标量 store 的编码宽度集
 *    只有 {1,2,4,8}，3/5 字节结构体在 `value_is_whole_copy` 下归入整块
 *    拷贝；交付侧 `va_arg_result_in_memory` 必须与同一判据同源，否则赋值
 *    路由按类型轴走通用物化，把 8 字节寄存器值存进目标下方 8 字节的临时
 *    区 —— 那里紧邻前一个变量，实测 GCC torture 931004-12.c /
 *    strct-stdarg-1.c 的循环计数器被整个覆盖（循环只跑一趟）。
 * 2) 空结构体（GNU 扩展，size 0）返回并赋值不得要求物化缓冲
 *    （GCC torture compile/920928-5.c 的函数指针参数形式）。
 *
 * 失败码：1/2=3 字节变参值或循环变量错，3=3 字节尾部 long 错，
 * 4=5 字节变参值错，5=5 字节尾部 long 错，6=空结构体返回路径。
 */
#include <stdarg.h>

struct tiny3 { char c, d, e; };
struct tiny5 { char c, d, e, f, g; };
struct empty {};

static int
take3 (int n, ...)
{
  va_list ap;
  int i;
  va_start (ap, n);
  for (i = 0; i < n; i++)
    {
      struct tiny3 x = va_arg (ap, struct tiny3);
      if (x.c != i + 10 || x.d != i + 20 || x.e != i + 30)
        return 1;
      /* i 若被物化 store 覆盖，本轮序号就错；取值同样会错。 */
      if (i != x.c - 10)
        return 2;
    }
  {
    long v = va_arg (ap, long);
    if (v != 123)
      return 3;
  }
  va_end (ap);
  return 0;
}

static int
take5 (int n, ...)
{
  va_list ap;
  int i;
  va_start (ap, n);
  for (i = 0; i < n; i++)
    {
      struct tiny5 x = va_arg (ap, struct tiny5);
      if (x.c != i + 10 || x.d != i + 20 || x.e != i + 30 || x.f != i + 40
          || x.g != i + 50)
        return 4;
    }
  {
    long v = va_arg (ap, long);
    if (v != 456)
      return 5;
  }
  va_end (ap);
  return 0;
}

/* 空结构体经函数指针参数返回：赋值不得去要物化缓冲地址。 */
static struct empty
ret_empty (void)
{
  struct empty e;
  return e;
}

static int
call_through (struct empty (*f) (void))
{
  struct empty d = f ();
  (void) d;
  return 0;
}

int
main (void)
{
  struct tiny3 a = {10, 20, 30}, b = {11, 21, 31}, c = {12, 22, 32};
  struct tiny5 p = {10, 20, 30, 40, 50}, q = {11, 21, 31, 41, 51};
  int rc;

  rc = take3 (3, a, b, c, 123L);
  if (rc)
    return rc;
  rc = take5 (2, p, q, 456L);
  if (rc)
    return rc;
  if (call_through (ret_empty) != 0)
    return 6;
  return 0;
}
