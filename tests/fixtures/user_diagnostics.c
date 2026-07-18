// tests/fixtures/user_diagnostics.c
// #warning / #error 用户自定义诊断。
// 本文件本身只用 #warning，不应中断编译。

#if defined(__has_warning)
#warning "__has_warning is defined (informational)"
#endif

#ifndef DCC_VERSION_GUARD
#warning "DCC_VERSION_GUARD is not defined (informational)"
#endif

int main(void) {
    return 0;
}
