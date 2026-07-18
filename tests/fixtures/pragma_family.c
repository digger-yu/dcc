// tests/fixtures/pragma_family.c
// #pragma pack / #pragma weak / #pragma STDC FENV_ACCESS / #pragma once / #pragma GCC diagnostic
// 当前阶段主要验证解析与静默接受，不一定触发实际行为变化。

#pragma once
#pragma pack(1)
struct Packed1 {
    char c;
    int i;
};
#pragma pack()
#pragma pack(2)
struct Packed2 {
    char c;
    int i;
};
#pragma pack()

#pragma weak weak_global
int weak_global = 42;
int use_weak(void) {
    return weak_global;
}

#pragma STDC FENV_ACCESS ON
double fp_op(double x) {
    return x * 2.0;
}

#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wunused-variable"
int unused_ok = 0;
#pragma GCC diagnostic pop

int main(void) {
    if (sizeof(struct Packed1) == (int)sizeof(char) + (int)sizeof(int)) {
        if (fp_op(1.5) == 3.0) {
            return 0;
        }
    }
    return 1;
}
