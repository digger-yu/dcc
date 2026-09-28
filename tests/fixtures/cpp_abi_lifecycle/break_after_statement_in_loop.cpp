// Stage 2/4 交叉验证: 循环体里「先做事再跳出循环」的块归属。
//
// SSA→AST 结构化重建按**路径**展开循环体, 目标是「循环体外」的分支过去被直接射成
// `break`, 于是分支体里排在 `break` 之前的语句整块丢失 (实测只打 `L` 而 g++ 打
// `LX`)。现在按「本块有语句 + 前驱全在循环内 + 不是 header 的正常出口」判定是否
// 就地展开; 最后一段同时验证**共享出口块不被展开** (否则循环后的语句会被执行两遍)。
#include <cstdio>

int main() {
  // 1. if 体内先做事再 break
  for (int i = 0; i < 3; i = i + 1) {
    if (i == 1) {
      printf("X");
      break;
    }
    printf("L");
  }
  printf("|");

  // 2. if 体内先做事再 continue (跳回 header 的路径同理不能丢语句)
  for (int i = 0; i < 3; i = i + 1) {
    if (i == 1) {
      printf("C");
      continue;
    }
    printf("%d", i);
  }
  printf("|");

  // 3. 嵌套循环: 内层 break 不能打断外层
  for (int i = 0; i < 2; i = i + 1) {
    for (int j = 0; j < 3; j = j + 1) {
      if (j == 1) {
        printf("b");
        break;
      }
      printf("%d%d", i, j);
    }
    printf(".");
  }
  printf("|");

  // 4. 两条分支都带语句, 汇合点在循环之后 (共享出口块只能执行一次)
  int n = 0;
  for (int i = 0; i < 4; i = i + 1) {
    if (i == 2) {
      n = n + 10;
      break;
    }
    if (i == 3) {
      n = n + 100;
      break;
    }
    n = n + 1;
  }
  printf("n=%d\n", n);
  return 0;
}
