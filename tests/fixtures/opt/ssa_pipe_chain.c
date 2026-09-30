/* P16 流水落地的端到端样本：三种判定共存。
 *
 * `ch` 与 `ch_store` 是同一迭代内串起来的依赖链，在当前资源模型下确实能拿到
 * 一级跳迭代重叠（depth=1）并通过闸门，因此它们会真的被 peel + kernel 重排；
 * `red` 是 load 归约，实测 depth=0，闸门必须拒它 —— 三种形态放在同一个翻译单元里，
 * 才能同时盯住「该流水的流水对了」和「不该流水的没被碰」。
 *
 * 默认路径（不开 DCC_PIPE_ON）下三档必须与 gcc 逐字节一致。打开 DCC_PIPE_ON 后
 * -O2 目前会得到 ch=8（应为 475027），即落地在真实管线里仍有错值：这是
 * docs/ssa-pass-upgrade-plan.md §7.5 记录的必修项，也是流水默认不接入的直接原因。
 */
#include <stdio.h>
static int arr[26];
/* 依赖链：会被软件流水（depth=1） */
int ch(int n){int a=0,b=0,c=0,d=0,e=0,f=0;for(int i=0;i<24;i++){a+=1;b+=a;c+=b;d+=c;e+=d;f+=e;}return f+n;}
/* 链 + 数组写：流水后 store 实例的写地址必须逐一下标覆盖，不多不少 */
int ch_store(int start){int a=0,b=0,c=0;for(int i=0;i<20;i++){a+=2;b+=a;c+=b;arr[i]=c;}int s=0;for(int i=0;i<20;i++)s+=arr[i];return s+start;}
/* 归约：实测 depth=0，闸门应拒它（同一程序里两种判定共存） */
int red(int n,int*a){int s=0;for(int i=0;i<n && i<8;i++)s+=a[i];return s;}
int main(void){
  int x[8]; for(int i=0;i<8;i++) x[i]=i*3;
  printf("ch=%d\n", ch(7));
  printf("ch_store=%d\n", ch_store(1));
  printf("red=%d\n", red(8,x));
  printf("arr[19]=%d arr[25]=%d\n", arr[19], arr[25]);
  return 0;
}
