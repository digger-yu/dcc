/*
 * tests/fixtures/c/system_header.c
 *
 * #pragma GCC system_header 支持测试:
 * - #pragma GCC system_header 前: 警告正常发出
 * - #pragma GCC system_header 后: 警告被抑制
 * - _Pragma("GCC system_header") 等价形式
 * - 不影响错误 (#error 仍然终止)
 */

/* 1. pragma 前的 #warning 应正常发出 */
#warning before system_header - should appear

/* 2. 标记为系统头文件 */
#pragma GCC system_header

/* 3. pragma 后的 #warning 应被抑制 */
#warning after system_header - should be suppressed

/* 4. 正常代码不受影响 */
int system_header_test_var = 42;

int system_header_main(void) {
    return system_header_test_var == 42 ? 0 : 1;
}
