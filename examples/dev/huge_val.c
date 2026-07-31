int main(void) {
    double x = __builtin_huge_val();
    return x != x;  // NaN returns 1, but Inf returns 0
}
