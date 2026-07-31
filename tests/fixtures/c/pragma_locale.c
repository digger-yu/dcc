// C23 #pragma locale 接受路径 (M19)
// 编译必须不报错且不影响正常代码生成
#pragma locale default
#pragma locale "POSIX"
#pragma locale reset
int main(void) {
    return 0;
}
