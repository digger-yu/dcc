/* m27_attr_warning.c - __attribute__((warning)) 警告测试 */
/* 调用带 warning 属性的函数应触发编译警告但不阻断 */

/* 声明带 warning 属性的函数 */
void deprecated_func(void) __attribute__((warning("deprecated_func is deprecated, use new_func instead")));

void deprecated_func(void) {
    /* 实现 */
}

int new_func(void) {
    return 42;
}

int main(void) {
    /* 调用应触发警告但继续编译 */
    deprecated_func();
    return new_func();
}