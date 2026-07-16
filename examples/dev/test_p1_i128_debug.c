// 调试 int128 减法 - 通过返回值判断
int main(void) {
    __int128 d = 100;
    __int128 e = 50;
    __int128 r = d - e;
    
    // 检查低 64 位
    long long lo = (long long)r;
    
    // 如果结果正确 (50)，返回 0
    // 如果结果错误，返回低位的值用于调试
    if (lo == 50) return 0;
    return (int)lo;
}
