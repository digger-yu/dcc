/* P16 流水落地的端到端样本：三种判定共存。
 *
 * `ch` 与 `ch_store` 是同一迭代内串起来的依赖链，在当前资源模型下确实能拿到
 * 一级跳迭代重叠（depth=1）并通过闸门，因此它们会真的被 peel + kernel 重排；
 * `red` 是 load 归约，实测 depth=0，闸门必须拒它 —— 三种形态放在同一个翻译单元里，
 * 才能同时盯住「该流水的流水对了」和「不该流水的没被碰」。
 *
 * 默认路径（不开 DCC_PIPE_ON）下三档必须与 gcc 逐字节一致。打开 DCC_PIPE_ON 后，
 * 下面两个 chain6_* 函数是**最小错值形态**（实测 m=6 且 t>=10 时 -O2 结果偏大，
 * 多出的量恰好等于上一级变量的终值 ⇒ 最后一个节点多累加了一轮），而 m<=5 全对。
 * 它们是流水落地能否默认打开的判据：修好后这整份夹具在 DCC_PIPE_ON=1 下也必须与
 * gcc 一致。详见 docs/ssa-pass-upgrade-plan.md §7.5。
 */
#include <stdio.h>
static int arr[26];
/* 依赖链：会被软件流水（depth=1） */
int ch(int n){int a=0,b=0,c=0,d=0,e=0,f=0;for(int i=0;i<24;i++){a+=1;b+=a;c+=b;d+=c;e+=d;f+=e;}return f+n;}
/* 链 + 数组写：流水后 store 实例的写地址必须逐一下标覆盖，不多不少 */
int ch_store(int start){int a=0,b=0,c=0;for(int i=0;i<20;i++){a+=2;b+=a;c+=b;arr[i]=c;}int s=0;for(int i=0;i<20;i++)s+=arr[i];return s+start;}
/* 归约：实测 depth=0，闸门应拒它（同一程序里两种判定共存） */
int red(int n,int*a){int s=0;for(int i=0;i<n && i<8;i++)s+=a[i];return s;}
/* 最小错值形态：6 级依赖链、trip=10。m<=5 全对，m=6 且 t>=10 才暴露。
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
