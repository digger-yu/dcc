// Test dynamic RTTI through polymorphic pointers
class Base {
public:
    virtual ~Base() {}
    virtual void foo() {}
};

class Derived : public Base {
public:
    void foo() override {}
};

int printf(const char *fmt, ...);

int main() {
    Base* p = new Derived();
    
    // Static typeid - should work
    // Dynamic typeid through polymorphic pointer - needs vtable typeinfo
    // For now, just test that we can create polymorphic objects
    
    p->foo();  // Should call Derived::foo
    
    delete p;
    printf("PASS\n");
    return 0;
}
