/* P16 真轮转寄存器（Phase 6.3）的端到端样本：被旧保守判据拒收、现在应被接受的循环。
 *
 * 旧判据只有一条：`src/ssa/pipelining.rs::benefit_verdict` 里的 `depth > 1` ⇒
 * 「需要超过一级轮转寄存器（本层不做）」。它护的是「SSA→AST 重建把同基名塌缩成
 * 一个栈槽」⇒ depth≥2 时一个基名要同时装两份不同的实例，拿不到就出错值（历史上
 * 炸出来的是 6 级链 -O2 得 7006 而真值 5005，见 docs/ssa-pass-upgrade-plan.md §7.6）。
 *
 * 现在的解法是**轮转格**：每个 (跳轮生产者, lag) 由 `LocMap::reserve_split_name` 分一个
 * 独立基名（形如 `b_s1`），它 own 一个 AST 局部，因此 own 一个自己的家（寄存器或帧内
 * 带里的一个槽），kernel 行尾有一条从大到小的拷贝链把它往前推一轮。于是「同一个基名
 * 的两份值」不再是同一个槽的两个用途，而是两个名字的两个槽。
 *
 * 实测的本档位行为（`DCC_PIPE_DUMP=1` 逐条复核，格子表就在 `rot_cells=` 那行）：
 *   * `rot_load4`：4 级依赖载入链、trip=32、depth=2 ⇒ 收，4 格
 *     `["b.1#5:lag1->b_s1", "_tidx_9#10:lag1->_tidx_9_s1", "s.2#12:lag1->s_s1", "i.2#15:lag1->i_s1"]`；
 *   * `rot_load5`：5 级链、trip=40、depth=2 ⇒ 收，4 格；
 *   * `rot_clash`：用户自己占用了 `i_s1` ⇒ 格子自动避让到 `i_s2`（占名分配器是全仓
 *     唯一那一份 `LocMap::reserve_split_name`，这里没有第二套命名判据）；
 *   * `rot_short`：与 `rot_load4` 同形状但 trip=4 ⇒ 仍拒，而拒因必须是**真原因**
 *     「trip 太短」，不再是指一条已经不存在的拦路虎；
 *   * `rot_flat`：6 级纯算术链、depth=1 ⇒ 老模型（格子表空 = 基名塌缩）就该收，
 *     它的产物字节是「本轮改动没有抖动老接受形状」的对照项。
 *   * `rot_chain2`：2 级载入链、depth=1 ⇒ 塌缩模型就收，但它护的是本轮从轮转里
 *     掘出的那条 `inline_temps` 缺陷（见下方函数上方的注释）。
 *
 * 关掉真轮转（`DCC_SSA_OFF=rot`）时前四个形状逐字退回旧文案
 * 「需要超过一级轮转寄存器（本层不做）」，只有 `rot_flat` 仍被收 —— 反向对照由
 * `tests/integration/p16_rotation.rs` 钉住。
 *
 * 全部四档必须与 gcc 逐字节一致：每个期望值都取自 gcc 实测，不是手算。
 */
#include <stdio.h>
static int idx[8];
/* 2 级依赖载入链：depth=1、塌缩模型就能收。它是本轮真轮转掘出来的**既存静默误编译**
 * 的最小形状（基线二进制同样错）：prologue 里 `_tbin = a & 7` 那条定义只被头块的
 * 轮转 phi 读，而 `src/ssa/inline_temps.rs` 的 `use_count`/`find_use_site` 不看 phi 入边
 * ⇒ 被当成「无人读 ⇒ 可内联」丢掉 ⇒ 那个槽从未被写 ⇒ -O2 得 372 而 gcc 四档都是 375。
 * 守卫在 `inline_temps.rs::read_by_phi`；判据见
 * `p16_rotation_every_slot_is_written_before_its_first_read`。*/
int rot_chain2(int n){int s=0;for(int i=0;i<32;i++){int a=idx[i&7]+1;int b=idx[a&7]+1;s+=b;}return s+n;}
/* 4 级依赖载入链：旧判据 depth>1 拒，真轮转收（4 个格子） */
int rot_load4(int n){int s=0;for(int i=0;i<32;i++){int a=idx[i&7]+1;int b=idx[a&7]+1;int c=idx[b&7]+1;int d=idx[c&7]+1;s+=d;}return s+n;}
/* 5 级：同一批格子数（跳轮读按 (生产者,lag) 归并，不与链长成比例） */
int rot_load5(int n){int s=0;for(int i=0;i<40;i++){int a=idx[i&7]+1;int b=idx[a&7]+1;int c=idx[b&7]+1;int d=idx[c&7]+1;int e=idx[d&7]+1;s+=e;}return s+n;}
/* 占名对照：用户已有局部 `i_s1`，而归纳变量 `i` 的格子恰好想要 `i_s1` */
int rot_clash(int n){int s=0,i_s1=5;for(int i=0;i<24;i++){int a=idx[i&7]+1;int b=idx[a&7]+1;int c=idx[b&7]+1;int d=idx[c&7]+1;s+=d;s+=i_s1;}return s+n;}
/* 同一形状但 trip 太短：仍应被拒（代价判据，不是能力判据） */
int rot_short(int n){int s=0;for(int i=0;i<4;i++){int a=idx[i&7]+1;int b=idx[a&7]+1;int c=idx[b&7]+1;int d=idx[c&7]+1;s+=d;}return s+n;}
/* depth=1 的老接受形状：模型 1（基名塌缩）就该收，不许因本轮改动而抖 */
int rot_flat(int n){int a=0,b=0,c=0,d=0,e=0,f=0;for(int i=0;i<24;i++){a+=1;b+=a;c+=b;d+=c;e+=d;f+=e;}return f+n;}
int main(void){
  for(int i=0;i<8;i++) idx[i]=i*3;
  printf("rot_chain2=%d\n", rot_chain2(7));
  printf("rot_load4=%d\n", rot_load4(7));
  printf("rot_load5=%d\n", rot_load5(7));
  printf("rot_clash=%d\n", rot_clash(7));
  printf("rot_short=%d\n", rot_short(7));
  printf("rot_flat=%d\n", rot_flat(7));
  return 0;
}
