extern "C" {
void* malloc(unsigned long n);
void free(void* p);
void* _Znwm(unsigned long n) { return malloc(n); }
void* _Znam(unsigned long n) { return malloc(n); }
void _ZdlPv(void* p) { free(p); }
void _ZdaPv(void* p) { free(p); }
}

int get_count() { return 7; }
char* get_str() { return 0; }
long get_big() { return 1000000L; }

class Widget {
public:
    int id;
    Widget() {}
    int get_id() { return id; }
};

// 两个 int 成员: 若 auto 推导/成员偏移错, b 会读到 a 的存储
class Pair {
public:
    int a;
    int b;
    Pair() : a(0), b(0) {}
    int sum() { return a + b; }
};

int main() {
    auto c = get_count();   // int (函数返回类型注册表)
    auto s = get_str();     // char* (函数返回类型注册表)
    auto g = get_big();     // long
    Widget w;
    w.id = 5;
    auto fid = w.id;        // int (成员字段)
    auto mid = w.get_id();  // int (成员调用)
    auto p = new Widget;    // Widget* (new 类型)
    p->id = 42;             // 经推导出的指针写成员
    auto pid = p->id;       // int (-> 成员访问)
    auto q = new Pair;      // Pair*
    q->a = 11;
    q->b = 31;
    auto qa = q->a;
    auto qb = q->b;
    auto qs = q->sum();
    auto arr = new int[4];  // int* (new[] 元素类型 -> 指针)
    arr[0] = 1;
    arr[1] = 2;
    arr[2] = 4;
    arr[3] = 8;
    auto e1 = arr[1];
    auto e3 = arr[3];

    int r = 0;
    // 推导出的类型尺寸必须与源类型一致 (退化成 int 会在这里暴露)
    if (sizeof(c) != 4) r |= 1;
    if (sizeof(s) != 8) r |= 2;
    if (sizeof(g) != 8) r |= 4;
    if (sizeof(p) != 8) r |= 8;
    if (sizeof(arr) != 8) r |= 16;
    if (sizeof(w) != 4) r |= 32;

    // 值本身也必须正确
    if (c != 7) r |= 64;
    if (g != 1000000L) r |= 128;
    if (s != 0) r |= 256;
    if (fid != 5 || mid != 5) r |= 512;
    if (p == 0 || pid != 42) r |= 1024;
    if (qa != 11 || qb != 31 || qs != 42) r |= 2048;
    if (e1 != 2 || e3 != 8) r |= 4096;

    delete p;
    delete q;
    delete[] arr;
    return r;
}
