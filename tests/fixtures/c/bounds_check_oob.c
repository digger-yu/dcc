// 数组下标运行时边界检查 (-fbounds-check), 越界路径触发 libubsan 诊断:
//   runtime error: index 5 out of bounds for type 'int [3]'
// 验证 OutOfBoundsData 布局与 gcc libsanitizer 一致 (可写节 + TypeInfo 编码),
// libubsan SourceLocation::acquire() 原子写 Column 不段错误。

#include <stdio.h>

int main(void) {
    int arr[3] = {10, 20, 30};
    int x = arr[5]; // 越界 (元素索引 5, 非字节偏移 20)
    printf("arr[5] = %d\n", x);
    return 0;
}
