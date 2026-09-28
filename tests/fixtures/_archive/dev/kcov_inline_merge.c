// examples/dev/kcov_inline_merge.c
// -fsanitize-coverage=trace-pc-guard: KCOV 跨函数 inline 合并
//
// 编译: dcc -fsanitize-coverage=trace-pc-guard -c kcov_inline_merge.c
//
// KCOV 跨函数 inline 合并:
// 预扫描识别 always_inline 函数, 当内联函数仅被单一调用者引用时
// 复用调用者的 guard 数组, 避免内联后覆盖计数重复。
//
// 处理顺序:
//   1. 预扫描: 收集 always_inline/flatten 函数及其调用者
//   2. 第一遍: 处理非内联函数, 分配独立 guard 数组
//   3. 第二遍: 处理内联函数, 复用调用者的 guard 数组名

// always_inline: 会被内联到 caller 中, guard 合并到 caller 的数组
__attribute__((always_inline))
static inline int helper_add(int a, int b) {
    return a + b;
}

// 普通函数: 独立 guard 数组
int compute(int x, int y) {
    // helper_add 被内联后, 其 guard 使用 compute 的 guard 数组
    int sum = helper_add(x, y);
    if (sum > 100) {
        return sum * 2;
    }
    return sum;
}

int main(void) {
    return compute(42, 58);
}
