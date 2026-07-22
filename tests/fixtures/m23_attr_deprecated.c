// M23: __attribute__((deprecated)) —— 解析时不报错
// 测试目标: 编译通过
__attribute__((deprecated("use new_func instead")))
void old_func(void) {}

void new_func(void) {}

int main(void) {
    new_func();
    return 42;
}
