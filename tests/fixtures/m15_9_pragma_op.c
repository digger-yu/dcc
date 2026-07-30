// tests/fixtures/pragma_operator.c
// _Pragma("...") 运算符等价于 #pragma ...
// 应在预处理阶段被识别并把内部字符串转发到 process_directive。

_Pragma("GCC diagnostic push")
_Pragma("GCC diagnostic ignored \"-Wunused-variable\"")
int ignored_var = 0;
_Pragma("GCC diagnostic pop")

_Pragma("pack(1)")
struct ViaPragma {
    char c;
    int i;
};
_Pragma("pack()")

int main(void) {
    if (sizeof(struct ViaPragma) == (int)sizeof(char) + (int)sizeof(int)) {
        return 0;
    }
    return 1;
}
