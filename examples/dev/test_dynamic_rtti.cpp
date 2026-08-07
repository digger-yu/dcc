// Test 1: Dynamic RTTI through polymorphic pointers
class Base {
public:
    virtual ~Base() {}
    virtual int getType() { return 1; }
};

class Derived : public Base {
public:
    int getType() override { return 2; }
};

int printf(const char *fmt, ...);

int main() {
    Base* p = new Derived();
    
    // Test virtual function call (should work)
    int t = p->getType();
    printf("Virtual call: %d (expected 2)\n", t);
    
    // Test typeid - this needs dynamic RTTI
    // For now, just verify compilation works
    
    delete p;
    return 0;
}
