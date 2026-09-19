// G11: Template template parameter support
// Validates parser can recognize and handle `template<typename...> class Name` syntax

#include <vector>
#include <deque>

// Primary template with template template parameter
template<typename T, template<typename> class Container>
class Adapter {
public:
    Container<T> data;
    
    void push(T value) {
        // Simplified - no runtime needed for parsing test
    }
};

// Specialization with different container type
template<template<typename> class Container>
class Adapter<void, Container> {
public:
    Container<void*> data;
};

// Test basic instantiation
int main() {
    // These should parse without errors
    Adapter<int, std::vector> v1;
    Adapter<double, std::deque> v2;
    
    (void)v1;
    (void)v2;
    
    return 0;
}
