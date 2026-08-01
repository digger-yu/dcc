// M18.x: SFINAE (substitution failure is not an error) 简化支持
// 覆盖:
//   1. 匿名默认类型形参 `typename = T` (enable_if 惯用法,
//      真实代码形如 `typename = enable_if_t<...>`)
//   2. 模板默认实参 (template <typename T = int>)
//   3. 多类型形参模板
// 求值层 (enable_if 条件 / trait 检测) 由 advanced.rs 单元测试覆盖.

// 1. SFINAE 惯用法: 匿名默认形参
template <typename T, typename = void>
int sfinae_anon(T x) { return (int)x; }

// 2. 默认模板实参
template <typename T = int>
int sfinae_default_arg() { return 42; }

// 3. 多形参模板
template <typename T, typename U>
T sfinae_first(T a, U b) { return a; }

int test_sfinae() {
    int a = sfinae_anon(7);
    int b = sfinae_default_arg();
    long c = sfinae_first(3L, 4.5);
    if (a != 7) return 1;
    if (b != 42) return 2;
    if (c != 3) return 3;
    return 0;
}

int main() { return test_sfinae(); }
