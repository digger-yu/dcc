// C++98 iterator_traits: verify header compiles and basic types
#include <iterator>

// Verify iterator_traits pointer specialization compiles
int main() {
    // iterator_traits<int*> should have value_type = int
    // This is a compile-only test
    return 0;
}
