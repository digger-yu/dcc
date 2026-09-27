// multi_declarator_for_init.cpp —— for-init 里的多 init-declarator ([stmt.init]/[stmt.for])
//
// 文法上 for-init 只能是**单个** declaration-statement, 所以
// `for (int i = 0, j = n - 1; ...)` 必须作为**一条**语句 (CppStmt::LocalVarList)
// 降级为 C 的 `for (int i = 0, j = n - 1; ...)`, 不能拆成两条声明语句 ——
// 拆开后就不再是合法的 for-init.
//
// 验证点 (退出码逐条与 g++ -std=c++20 真值对照):
//   1) 双计数器夹逼: i 升 j 降, 循环次数与两个计数器的值都正确;
//   2) 声明只在进入循环时执行一次, 第二个 declarator 的初值参与条件比较;
//   3) for-init 里只有第一个 declarator 带 `*` 时, 后续仍是值类型
//      (`for (int *p = &x, n = 3; ...)`: n 是 int, 不是 int*);
//   4) 无初始化的 for-init (`for (int i, n = 4; ...)`) 也必须是合法一条语句.
int clamp_sum(int n) {
    int acc = 0;
    for (int i = 0, j = n - 1; i < j; i++, j--) {
        acc = acc + i * 10 + j;
    }
    return acc;
}

int step_two(int n) {
    int acc = 0;
    for (int i = 0, j = 0; i < n; i++, j = j + 2) {
        acc = acc + i + j;
    }
    return acc;
}

int ptr_init(int x) {
    for (int *p = &x, n = 3; n > 0; n--) {
        x = x + 1 + (p == &x ? 0 : 100);
    }
    return x;
}

int uninit_init(int n) {
    int acc = 0;
    for (int i, k = 0; k < n; k++) {
        i = k * 2;
        acc = acc + i;
    }
    return acc;
}

int main() {
    if (clamp_sum(5) != 17) return 1;
    if (clamp_sum(4) != 15) return 2;
    if (clamp_sum(1) != 0) return 3;
    if (step_two(4) != 18) return 4;
    if (ptr_init(10) != 13) return 5;
    if (uninit_init(4) != 12) return 6;
    return 0;
}
