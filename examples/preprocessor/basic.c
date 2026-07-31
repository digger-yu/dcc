// 预处理器功能测试
// 测试: #define, #if, #ifdef, 字符串化, 令牌拼接, 可变参数宏, __FILE__, __LINE__

#define STRINGIFY(x) #x
#define PASTE(a, b) a##b
#define MAX(a, b) ((a) > (b) ? (a) : (b))

// 可变参数宏
#define LOG(fmt, ...) fmt

int main() {
    // 基本宏展开
    int x = MAX(3, 5);
    
    // 字符串化
    char *s = STRINGIFY(hello);
    
    // 令牌拼接
    int var1 = 42;
    int PASTE(var, 1) = 99;  // 等价于 int var1 = 99;
    
    // __LINE__ 和 __FILE__
    int line = __LINE__;
    
    // #if 条件编译
#if defined(__DCC__)
    int dcc = 1;
#else
    int dcc = 0;
#endif

    return x;  // 应返回 5
}
