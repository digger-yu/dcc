// Test __attribute__((returns_nonnull))
// 运行时检查: 返回 NULL 会触发 UD2 指令导致程序 abort

#include <stdio.h>
#include <stdlib.h>

// 正常情况: 返回非空指针
__attribute__((returns_nonnull))
void* allocate_good(size_t size) {
    void* p = malloc(size);
    if (!p) {
        // 分配失败时返回一个有效指针而不是 NULL
        // 这样不会触发 returns_nonnull 的运行时检查
        return (void*)0x1000; // 模拟一个有效地址
    }
    return p;
}

// 错误情况: 故意返回 NULL (会触发 UD2)
__attribute__((returns_nonnull))
void* allocate_bad(size_t size) {
    (void)size;
    return NULL; // 这会触发运行时检查!
}

int main() {
    printf("Testing returns_nonnull attribute...\n");
    
    // 正常调用
    void* p1 = allocate_good(100);
    printf("allocate_good(100) = %p\n", p1);
    
    if (p1 == NULL) {
        printf("FAIL: allocate_good returned NULL\n");
        return 1;
    }
    
    printf("PASS: returns_nonnull function returned non-NULL\n");
    
    // 注意: 如果调用 allocate_bad(), 程序会 abort
    // void* p2 = allocate_bad(100); // 取消注释会导致程序崩溃
    
    return 0;
}
