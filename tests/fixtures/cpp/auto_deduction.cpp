extern "C" {
void* malloc(unsigned long n);
void* _Znwm(unsigned long n) { return malloc(n); }
}

int get_count() { return 7; }
char* get_str() { return 0; }

class Widget {
public:
    int id;
    Widget() {}
    int get_id() { return id; }
};

int main() {
    auto c = get_count();   // int (函数返回类型注册表)
    auto s = get_str();     // char* (函数返回类型注册表)
    Widget w;
    w.id = 5;
    auto fid = w.id;        // int (成员字段)
    auto mid = w.get_id();  // int (成员调用)
    auto p = new Widget;    // Widget* (new 类型)
    int r = 0;
    if (c != 7) r |= 1;
    if (s != 0) r |= 2;
    if (fid != 5) r |= 4;
    if (mid != 5) r |= 8;
    if (p == 0) r |= 16;
    return r;
}
