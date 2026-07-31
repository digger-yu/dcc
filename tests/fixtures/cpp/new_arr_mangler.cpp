// P2.4: operator new[] / operator delete[] 走 _Znam / _ZdaPv
// (Section 6). 验证 dcc lower 阶段对 `new T[n]` 和 `delete[] p`
// 调用 _Znam / _ZdaPv (Itanium C++ ABI 数组分配/释放符号),
// 不再使用硬编码字符串 (现走 mangle_op_new_array /
// mangle_op_delete_array 命名常量).
struct Item {
    int v;
};
int test_array_new_delete() {
    // 数组 new 走 _Znam(sizeof(struct Item) * 4)
    Item* arr = new Item[4];
    arr[0].v = 10;
    arr[1].v = 20;
    arr[2].v = 30;
    arr[3].v = 40;
    int s = arr[0].v + arr[1].v + arr[2].v + arr[3].v;
    // 数组 delete 走 _ZdaPv(arr)
    delete[] arr;
    return s; // 期望 100
}
