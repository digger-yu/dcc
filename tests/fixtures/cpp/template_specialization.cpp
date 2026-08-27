// M27: 模板偏特化选择 - specialization 排序
// 验证: 多个偏特化匹配时, 选最特化 (most specialized) 的那个
// 精确匹配 > 指针模式匹配 > 通配符匹配

template <typename T> struct Foo {
    int which() { return 0; }  // 主模板
};

// 偏特化 1: 指针类型
template <typename T> struct Foo<T*> {
    int which() { return 1; }  // T* 偏特化
};

// 偏特化 2: int 精确匹配
template <> struct Foo<int> {
    int which() { return 2; }  // int 全特化
};

int test_primary_template() {
    Foo<double> f;
    return f.which();  // 期望 0 (主模板)
}

int test_pointer_specialization() {
    Foo<int*> f;
    return f.which();  // 期望 1 (T* 偏特化)
}

int test_exact_specialization() {
    Foo<int> f;
    return f.which();  // 期望 2 (int 全特化)
}

int main() {
    int r = 0;
    if (test_primary_template() != 0) r |= 1;
    if (test_pointer_specialization() != 1) r |= 2;
    if (test_exact_specialization() != 2) r |= 4;
    return r;
}
