// 最简单的 int128 减法
int main(void) {
    __int128 a = 100;
    __int128 b = 50;
    __int128 c = a - b;
    
    // 直接比较 c 和 50
    if (c != 50) return 1;
    
    return 0;
}
