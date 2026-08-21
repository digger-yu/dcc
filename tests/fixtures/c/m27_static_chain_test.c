/* m27_static_chain_test.c - __builtin_call_with_static_chain 运行时测试 */
/* 验证: 嵌套函数通过静态链访问外层变量 */

int test_static_chain(void) {
    int outer_val = 21;
    int inner(void) {
        return outer_val * 2;
    }
    /* 传递当前帧指针作为静态链, inner 通过此链访问 outer_val */
    return __builtin_call_with_static_chain(inner, (int)__builtin_frame_address(0));
}

int main(void) {
    int r = test_static_chain();
    /* inner 应通过静态链访问 outer_val, 返回 21*2=42 */
    return r == 42 ? 0 : 1;
}