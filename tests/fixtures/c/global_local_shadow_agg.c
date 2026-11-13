/* 局部/参数遮蔽同名全局变量时的聚合/向量/__int128 寻址。
 *
 * GCC torture ipa/20040703-wpa.c: `static cpp_num num_lshift (cpp_num num, ...)`
 * 的参数 num 与文件作用域全局 num 同名, 函数内 `orig = num` 必须从参数槽
 * （rdi/rsi）取源。dcc 的聚合赋值曾用裸 `is_global_var(name)` 选址, 只查
 * 全局表就 emit_global_addr —— 参数被绑到全局符号, -O2 实测 res.low 错值。
 *
 * 同族站点（向量元素读写、__int128 装载/存储、per-cpu builtin 取址）共用
 * 「局部优先」判据 `is_global_not_local`。
 */

typedef struct {
  unsigned high, low, unsignedp, overflow;
} S;

S num = {0, 3, 0, 0};
S gs = {7, 9, 0, 0};
S gres;

__attribute__((noinline)) void
copy_through_param (S num, unsigned p, unsigned q)
{
  S orig;
  orig = num;
  if (p > 32)
    orig.high += q;
  if (q)
    orig.low <<= q;
  gres = orig;
}

typedef int v4 __attribute__((vector_size (16)));
v4 gv = {1, 2, 3, 4};

__attribute__((noinline)) int
vec_elem_through_param (v4 gv)
{
  return gv[2];
}

__int128 gi = 5;

__attribute__((noinline)) int
int128_through_param (__int128 gi)
{
  return (int) gi;
}

int
main (void)
{
  copy_through_param (gs, 64, 16);
  if (gres.high != 23 || gres.low != 0x90000)
    return 1;

  {
    v4 x = {5, 6, 7, 8};
    if (vec_elem_through_param (x) != 7)
      return 2;
  }

  if (int128_through_param (9) != 9)
    return 3;

  return 0;
}
