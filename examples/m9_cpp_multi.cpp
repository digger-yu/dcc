// M9 C++ 多类测试 (无继承, 验证方法调用改写)
class Counter {
public:
    Counter() : n_(0) {}
    void inc() { n_ = n_ + 1; }
    int get() const { return n_; }
private:
    int n_;
};

class Doubler {
public:
    Doubler() : x_(1) {}
    int twice() const { return x_ * 2; }
private:
    int x_;
};

int main() {
    Counter c;
    c.inc();
    c.inc();
    c.inc();
    Doubler d;
    return c.get() + d.twice() - 5;  // 3 + 2 - 5 = 0
}
