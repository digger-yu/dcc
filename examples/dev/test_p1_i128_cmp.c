// 测试 int128 表达式与 int 比较
int main(void) {
    __int128 d = 100;
    __int128 e = 50;
    __int128 r = d - e;
    
    // 测试 1: int128 变量与 int 比较
    if (r != 50) return 1;
    
    return 0;
}
