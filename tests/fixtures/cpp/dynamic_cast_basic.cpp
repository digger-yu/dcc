// Stage 7 RTTI: dynamic_cast 运行期 (upcast / downcast / crosscast / null 四分支)。
//
// dcc 只负责把 `dynamic_cast<T*>(p)` 降级为对 libstdc++ `__dynamic_cast` 的调用,
// 真正的类型遍历由 libstdc++ 按 Itanium ABI 读 dcc 发射的 typeinfo 对象
// (`__class_type_info` / `__si_class_type_info` / `__vmi_class_type_info` 的
// `__name` / `__base_info` 字段) 与其 vptr 指向的 `type_info` 虚表槽完成 ——
// 因此本夹具必须与 g++ 原生编译运行的退出码完全一致, 才说明两侧二进制布局互操作
// 成功。每个分支单独成函数, 由返回值位掩码汇总, 期望整体退出码 0。
struct Base {
    virtual ~Base() {}
    virtual int id() { return 1; }
};

struct Other {
    virtual ~Other() {}
    virtual int oid() { return 100; }
};

struct Mid : Base {
    int id() override { return 2; }
};

// 多继承: Mid(->Base) 在偏移 0, Other 在偏移 8, 交叉转换必须带 this 调整。
struct Leaf : Mid, Other {
    int id() override { return 3; }
    int oid() override { return 200; }
};

// 1) upcast: 基类指针 -> 派生侧基类指针, 结果非空且虚调用仍绑定 Leaf 的重写。
int check_upcast() {
    Leaf leaf;
    Base* bp = &leaf;
    Mid* mp = dynamic_cast<Mid*>(bp);
    if (!mp) return 1;
    if (mp->id() != 3) return 2;
    return 0;
}

// 2) downcast: 真实类型为 Leaf 的基类指针 -> Leaf*, 必须成功。
int check_downcast_ok() {
    Leaf leaf;
    Base* bp = &leaf;
    Leaf* lp = dynamic_cast<Leaf*>(bp);
    if (!lp) return 1;
    if (lp->id() != 3) return 2;
    return 0;
}

// 3) downcast 失败: 真实类型就是 Base 本身, 转 Leaf* 必须返回 null。
int check_downcast_fail() {
    Base base;
    Leaf* lp = dynamic_cast<Leaf*>(&base);
    if (lp) return 1;
    return 0;
}

// 4) crosscast: 经 Leaf 从 Base* 交叉到 Other*, 非空且 this 已调整到 Other 子对象。
int check_crosscast() {
    Leaf leaf;
    Base* bp = &leaf;
    Other* op = dynamic_cast<Other*>(bp);
    if (!op) return 1;
    if (op->oid() != 200) return 2;
    // 反向交叉: Other* -> Base* 同样要走通
    Base* back = dynamic_cast<Base*>(op);
    if (!back) return 4;
    if (back->id() != 3) return 8;
    return 0;
}

// 5) 空指针: 任何 dynamic_cast 结果都必须为 null (不得解引用虚表)。
int check_null_pointer() {
    Base* np = 0;
    int r = 0;
    if (dynamic_cast<Leaf*>(np)) r |= 1;
    if (dynamic_cast<Other*>(np)) r |= 2;
    if (dynamic_cast<Base*>(np)) r |= 4;
    Other* onp = 0;
    if (dynamic_cast<Mid*>(onp)) r |= 8;
    return r;
}

int main() {
    // 退出码只用低 5 位 (进程退出码截断到 8 位, 位移不得越过 bit7)。
    int r = 0;
    if (check_upcast()) r |= 1;
    if (check_downcast_ok()) r |= 2;
    if (check_downcast_fail()) r |= 4;
    if (check_crosscast()) r |= 8;
    if (check_null_pointer()) r |= 16;
    return r;
}
