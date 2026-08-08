// tests/fixtures/c/c11_atomic_freestanding.c
// freestanding 模式: __STDC_NO_ATOMICS__=1, __STDC_HOSTED__=0
// atomic_* 类型宏不应被预置

#if defined(__STDC_HOSTED__) && __STDC_HOSTED__ == 0
int hosted_is_0 = 1;
#endif

#if defined(__STDC_NO_ATOMICS__) && __STDC_NO_ATOMICS__ == 1
int no_atomics_is_1 = 1;
#endif

int main(void) {
    return (hosted_is_0 == 1 && no_atomics_is_1 == 1) ? 0 : 1;
}
