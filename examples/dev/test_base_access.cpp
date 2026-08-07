// Test 2: __base_ field access
class Base {
public:
    int x;
};

class Derived : public Base {
public:
    int y;
};

int printf(const char *fmt, ...);

int main() {
    Derived d;
    
    // Try to access inherited member
    d.x = 42;  // This should work but may fail with E1009
    d.y = 100;
    
    printf("d.x = %d, d.y = %d\n", d.x, d.y);
    
    return 0;
}
