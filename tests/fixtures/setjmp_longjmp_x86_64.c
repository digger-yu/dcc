// tests/fixtures/setjmp_longjmp_x86_64.c
// Section 11.5: setjmp CFI 寄存器保存表
// 验证 dcc 编译的 setjmp/longjmp 调用在运行时能正确恢复
// callee-saved 寄存器 (dcc 用 r13 当 frame pointer, 此处验证 r13
// 被 longjmp 正确恢复后, 函数返回到正确的位置).

#include <setjmp.h>

static jmp_buf env;
static volatile int marker;

static int check_r13_via_longjmp(int target) {
    // 通过 setjmp / longjmp 跨函数跳转, 验证 CFA / r13 恢复正确
    if (setjmp(env) == 0) {
        // setjmp 首次返回; 设置一个值, 然后跳到 target
        marker = 42;
        longjmp(env, target);
        return -1; // 不可达
    }
    // longjmp 后再次进入
    return marker + target;
}

int main(void) {
    // 测试 1: setjmp 首次返回 0, 调用 longjmp 后再次进入, 应为 marker + target
    int r = check_r13_via_longjmp(7);
    if (r != 42 + 7) return 1;
    // 测试 2: setjmp 在循环中多次 longjmp
    marker = 100;
    if (setjmp(env) == 0) {
        marker = 200;
        longjmp(env, 1);
    }
    if (marker + 1 != 201) return 2;
    return 0;
}
