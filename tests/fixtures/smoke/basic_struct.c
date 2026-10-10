// 测试struct成员访问
struct Point {
    int x;
    int y;
};

int main() {
    struct Point p;
    p.x = 10;
    p.y = 20;
    
    int sum = p.x + p.y;  // 30
    
    // 测试指针访问
    struct Point *ptr = &p;
    int px = ptr->x;  // 10
    int py = ptr->y;  // 20
    
    return (sum - px - py);  // 30 - 10 - 20 = 0
}
