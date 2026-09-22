/* 外部调用探针: 必须留下一个 UND 符号 `callee`, 否则说明调用被静默吞掉 */
int callee(int);
int caller(int x) { return callee(x) + callee(x ^ 1); }
