// Stage 4 交叉验证: 自动存储期对象的析构时机 (C++ [basic.stc.auto] + [stmt.return]
// + [stmt.jump])。
//
// 早前原生后端把 Destroy 一律接在**语句表末尾**, 于是函数体里
// `A a; return 0;` 一个析构也不跑 (对象从未销毁); 而只在嵌套块里声明的对象才会
// 被销毁 —— 老夹具恰好都写在嵌套块里, 所以这个缺口一直没被发现。现在析构在
// **声明点**登记进作用域帧, 块末 / `return` / `break` / `continue` 三条出口共用
// 同一批语句 (见 sema 的 `DtorFrame`)。
//
// 观察值全部由 printf 打出, 与 g++ 逐行对比。
#include <cstdio>

static char trace[128];
static int tn = 0;
static void note(char c) {
  if (tn < 126) {
    trace[tn++] = c;
  }
  trace[tn] = 0;
}

// 带析构的简单类: 每个实例在析构时打一个字母
struct Mark {
  char tag;
  explicit Mark(char t) : tag(t) { note('a' + (t - 'A')); }
  ~Mark() { note(tag); }
};

// 1) 函数体直接声明 (不在嵌套块里) —— 返回前必须析构, 逆序
int f_return() {
  Mark x('A');
  Mark y('B');
  return 0;
}

// 2) 嵌套块: 块末析构, 之后函数自己的局部量在返回前析构
int f_nested() {
  Mark outer('C');
  {
    Mark inner('D');
    note('X');
  }
  note('Y');
  return 0;
}

// 3) 循环体: 迭代结束时体内局部量必须析构; continue 同样先析构体内局部量
int f_loop(int mode) {
  Mark keep('E');
  for (int i = 0; i < 3; i = i + 1) {
    Mark body('F');
    if (mode == 2 && i == 1) {
      note('C');
      continue;
    }
    note('L');
  }
  return 0;
}

// 3b) break 离开循环体: 体内已构造的局部量在跳转前析构
// 注: 本例把 `break` 平铺在循环体里 —— `if (c) { stmt; break; }` 这种「then 分支
// 以 break 结尾」的写法在 lower_if 的块布局上有一个**与析构无关的既存缺陷**
// (分支体语句被跳过, 见方案文档 §6 已知缺陷), 不属于本夹具要验的析构语义。
int f_break() {
  Mark keep('E');
  for (int i = 0; i < 3; i = i + 1) {
    Mark body('F');
    note('L');
    break;
  }
  return 0;
}

// 4) 多个局部量 + 提前 return 的分支
int f_early(int flag) {
  Mark p('G');
  Mark q('H');
  if (flag != 0) {
    note('T');
    return 1;
  }
  note('N');
  return 0;
}

int main() {
  f_return();
  printf("t1=%s\n", trace);
  tn = 0;

  f_nested();
  printf("t2=%s\n", trace);
  tn = 0;

  f_loop(0);
  printf("t3=%s\n", trace);
  tn = 0;

  f_loop(1);
  printf("t4=%s\n", trace);
  tn = 0;

  f_loop(2);
  printf("t5=%s\n", trace);
  tn = 0;

  f_break();
  printf("t8=%s\n", trace);
  tn = 0;

  f_early(1);
  printf("t6=%s\n", trace);
  tn = 0;

  f_early(0);
  printf("t7=%s\n", trace);
  return 0;
}
