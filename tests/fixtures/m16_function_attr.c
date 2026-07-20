// m16_function_attr.c
// 函数属性传播: cold / hot 标注应被保留且不影响正确性
// 通过: exit 0

__attribute__((cold)) int cold_path(int x) {
    return x + 100;
}

int hot_path(int x) {
    return x * 2;
}

int main() {
    if (hot_path(21) != 42) return 1;
    if (cold_path(-50) != 50) return 2;
    return 0;
}
