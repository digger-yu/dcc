// template_nested_call_arg.cpp —— 实参**本身**是待实例化的函数模板调用
//
// 旧行为: 实例化工作表只登记最外层调用, 内层 `pick(5)` 停在原处 ->
// 链接期 `__dcc_tplfn_pick_*` undefined (modules 域曾以"跨 TU 才复现"绕过,
// 实际无模块的单个普通 TU 同样失败, 故本夹具不带任何 module 语法).
//
// 验证点 (退出码逐条与 g++ -std=c++20 真值对照):
//   1) 同一实例自嵌套 `pick(pick(5))`;
//   2) 深嵌套 4 层;
//   3) 内层与外层是**不同**函数模板 (`twice(pick(4))` / `pick(twice(3))`);
//   4) 嵌套调用作实参再喂给第三个模板 (`add(pick(2), twice(3))`);
//   5) 返回值参与后续运算不丢实例化 (`pick(pick(6)) + 1`).
//      (6 经两层 pick 仍是 6, +1 = 7)
template <typename T>
T pick(T v) {
    return v;
}

template <typename T>
T twice(T v) {
    return v * 2;
}

template <typename T>
T add(T x, T y) {
    return x + y;
}

int main() {
    if (pick(pick(5)) != 5) return 1;
    if (pick(pick(pick(pick(9)))) != 9) return 2;
    if (twice(pick(4)) != 8) return 3;
    if (pick(twice(3)) != 6) return 4;
    if (add(pick(2), twice(3)) != 8) return 5;
    if (pick(pick(6)) + 1 != 7) return 6;
    return 0;
}
