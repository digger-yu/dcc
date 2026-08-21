/* m27_attr_ctor_priority_invalid.c - constructor priority 负向测试 */
/* 验证: 超大的 priority 值应产生诊断 */

/* constructor priority 超出合理范围 */
void __attribute__((constructor(65536))) init_invalid(void) {
}

int main(void) {
    return 0;
}