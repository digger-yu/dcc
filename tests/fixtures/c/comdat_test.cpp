// C++ template instantiation generates COMDAT groups.
// Each instantiation of template<T> goes into a .text.<mangled> section
// grouped under a .group section with the mangled name as signature.
template <typename T>
T add(T a, T b) {
    return a + b;
}

int call_int_add(int a, int b) {
    return add<int>(a, b);
}

double call_double_add(double a, double b) {
    return add<double>(a, b);
}
