/* m27_attr_error.c - __attribute__((error)) 负向测试 */
/* 调用带 error 属性的函数应触发编译错误 */

/* 声明带 error 属性的函数 */
void forbidden_func(void) __attribute__((error("forbidden_func is not allowed")));

int main(void) {
    /* 调用此函数应触发编译错误 */
    forbidden_func();
    return 0;
}