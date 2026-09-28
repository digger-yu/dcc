// 测试 int128 减法 - 直接检查
int main(void) {
    __int128 d = 100;
    __int128 e = 50;
    
    // 直接检查 d
    long long d_lo = (long long)d;
    if (d_lo != 100) return 1;
    
    // 直接检查 e
    long long e_lo = (long long)e;
    if (e_lo != 50) return 2;
    
    // 检查 d - e
    __int128 r = d - e;
    long long r_lo = (long long)r;
    if (r_lo != 50) return 3;
    
    return 0;
}
