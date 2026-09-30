/* P16 流水落地的端到端样本：该流水与不该流水的形态共存（流水已默认接入 -O2/-O3）。
 *
 * 实测的本档位行为（`DCC_PIPE_DUMP=1` 可逐条复核，行尾就是拒因）：
 *   * `ch`、`chain6_10`：6 级依赖链 + 编译期已知 trip ⇒ 真被流水（depth=1）。
 *   * `chain5_24`：5 级链 ⇒ depth=0（最后一节点的 stage 仍在本行内）⇒ 拒。
 *   * `chain6_6`：trip=6 < 2*II ⇒ 「prologue/epilogue 开销盖过收益」⇒ 拒。
 *   * `red`：带 `i<n` 的运行期边界 ⇒ depth=0 ⇒ 拒。
 *   * `ch_store`：归纳变量初值不是区域外的唯一常量定义 ⇒ 拒（所以 store 实例的
 *     写地址形状当前只被准入护住，而不是被发射序自检护住）。
 *
 * 全部四档必须与 gcc 逐字节一致。曾经 `chain6_10` 是最小错值形态（-O2 得 7006，
 * 真值 5005）：kernel 行把链尾 `f += e` 排在 `e += d` 之后，而 SSA→AST 重建把同基名
 * 塌缩成一个槽 ⇒ f 读到本轮的 e。根因与修复见 docs/ssa-pass-upgrade-plan.md §7.5/§7.6。
 */
#include <stdio.h>
static int arr[26];
/* 依赖链：会被软件流水（depth=1） */
int ch(int n){int a=0,b=0,c=0,d=0,e=0,f=0;for(int i=0;i<24;i++){a+=1;b+=a;c+=b;d+=c;e+=d;f+=e;}return f+n;}
/* 链 + 数组写：当前被准入拒收（见顶部），作为「错值形态不得回潮」的守门项留着 */
int ch_store(int start){int a=0,b=0,c=0;for(int i=0;i<20;i++){a+=2;b+=a;c+=b;arr[i]=c;}int s=0;for(int i=0;i<20;i++)s+=arr[i];return s+start;}
/* 归约：实测 depth=0，闸门应拒它（同一程序里两种判定共存） */
int red(int n,int*a){int s=0;for(int i=0;i<n && i<8;i++)s+=a[i];return s;}
/* 曾经的错值形态：6 级依赖链、trip=10（5 级链实测 depth=0 不会被流水）。
 * trip 必须写成**字面常量**：流水准入要求编译期已知迭代次数，把它做成函数参数
 * 就根本不会被流水，夹具也就无法暴露任何东西（第 16 轮就踩过这个坑）。
 * chain6_6（小 trip）与 chain5_24（5 级、同量级结果）都是对照项。*/
int chain6_10(void){int a=0,b=0,c=0,d=0,e=0,f=0;for(int i=0;i<10;i++){a+=1;b+=a;c+=b;d+=c;e+=d;f+=e;}return f;}
int chain6_6(void){int a=0,b=0,c=0,d=0,e=0,f=0;for(int i=0;i<6;i++){a+=1;b+=a;c+=b;d+=c;e+=d;f+=e;}return f;}
int chain5_24(void){int a=0,b=0,c=0,d=0,e=0;for(int i=0;i<24;i++){a+=1;b+=a;c+=b;d+=c;e+=d;}return e;}
int main(void){
  int x[8]; for(int i=0;i<8;i++) x[i]=i*3;
  printf("ch=%d\n", ch(7));
  printf("ch_store=%d\n", ch_store(1));
  printf("red=%d\n", red(8,x));
  printf("arr[19]=%d arr[25]=%d\n", arr[19], arr[25]);
  printf("chain6_10=%d\n", chain6_10());
  printf("chain6_6=%d\n", chain6_6());
  printf("chain5_24=%d\n", chain5_24());
  return 0;
}
