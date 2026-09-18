// M40: Enhanced SFINAE - void_t and enable_if::type support (simplified)
// Tests for std::enable_if, std::void_t patterns used in libstdc++

// Define basic traits
struct true_type { enum { value = 1 }; };
struct false_type { enum { value = 0 }; };

// is_integral trait
template<typename T> struct is_integral : false_type {};
template<> struct is_integral<int> : true_type {};
template<> struct is_integral<long> : true_type {};

// is_floating_point trait  
template<typename T> struct is_floating_point : false_type {};
template<typename T> struct is_pointer : false_type {};
template<typename T> struct is_pointer<T*> : true_type {};

// is_same trait
template<typename T, typename U> struct is_same : false_type {};
template<typename T> struct is_same<T, T> : true_type {};

// enable_if definition
template<bool B, typename T = void> struct enable_if {};
template<typename T> struct enable_if<true, T> { typedef T type; };
template<typename T> using enable_if_t = typename enable_if<true, T>::type;

// Test 1: Basic enable_if with ::type member access
template<typename T, typename = typename enable_if<is_integral<T>::value, void>::type>
int test_enable_if_int(T x) {
    return (int)x;
}

template<typename T, typename = typename enable_if<is_floating_point<T>::value, void>::type>
double test_enable_if_float(T x) {
    return x * 2.0;
}

// Test 2: enable_if_t syntax
template<typename T>
typename enable_if<is_pointer<T>::value, int>::type
test_enable_if_t_ptr(T p) {
    return 8; // sizeof(void*) on 64-bit
}

template<typename T>
typename enable_if<!is_pointer<T>::value, int>::type
test_enable_if_t_nonptr(T x) {
    return (int)(x + 1);
}

// Test 3: void_t detection pattern (common in type traits)
template<typename...>
struct void_t_impl { using type = void; };

template<typename... Ts>
using void_t = typename void_t_impl<Ts...>::type;

// Detection idiom with void_t
template<typename T, typename = void>
struct has_value_type : false_type {};

template<typename T>
struct has_value_type<T, void_t<typename T::value_type>> : true_type {};

// Test 4: Complex enable_if with multiple conditions
template<typename T>
typename enable_if<
    is_same<T, int>::value || 
    is_same<T, long>::value,
    T
>::type
test_multi_condition(T x) {
    return x;
}

int test_enhanced_sfinae() {
    // Should pick integral version
    int a = test_enable_if_int(7);
    if (a != 7) return 1;
    
    // Should pick float version  
    double b = test_enable_if_float(3.14);
    if ((int)(b - 6.28) != 0) return 2;
    
    // Pointer case
    int c = test_enable_if_t_ptr((int*)0);
    if (c != 8) return 3;
    
    // Non-pointer case
    int d = test_enable_if_t_nonptr(5);
    if (d != 6) return 4;
    
    // Multi-condition
    int e = test_multi_condition(42);
    if (e != 42) return 5;
    
    return 0;
}

int main() { return test_enhanced_sfinae(); }
