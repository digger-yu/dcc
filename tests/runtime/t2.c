/* exec 测试的被执行体：打印固定一行后以退出码 7 退出，
 * 父进程用 wait4 校验 WIFEXITED/WEXITSTATUS —— 覆盖
 * fork → execve → 退出码回传的完整链路。 */
#include <unistd.h>

int main(void) {
    static const char line[] = "RT|t2|alive\n";
    if (write(1, line, sizeof(line) - 1) != (long)sizeof(line) - 1)
        return 111;
    return 7;
}
