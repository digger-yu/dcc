// C++98/11: destructors are implicitly noexcept
// Verify: destructor without explicit noexcept gets __attribute__((nothrow))
struct Foo {
    int x;
    Foo() : x(0) {}
    ~Foo() {}  // no explicit noexcept, but should be nothrow
};

struct Bar {
    Foo f;
    Bar() {}
    ~Bar() {}  // implicit noexcept per C++11
};

int main() {
    Bar b;
    return 0;
}
