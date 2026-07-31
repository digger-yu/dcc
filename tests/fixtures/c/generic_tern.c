int generic_ternary(void) {
  /* glibc string.h 常见模式：_Generic 控制表达式含三元运算符 */
  return _Generic(0 ? (const char *)"x" : (void *)0,
                  const void *: 1,
                  default: 2);
}
