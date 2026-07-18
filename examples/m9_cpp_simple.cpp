// M9 C++ 简单测试: 显式 this-> 不需要 method call
class Counter {
public:
    Counter() : n_(0) {}
    void inc() { this->n_ = this->n_ + 1; }
    int get() const { return this->n_; }
private:
    int n_;
};

int main() {
    Counter c;
    c.inc();
    c.inc();
    c.inc();
    return c.get() - 3;
}
