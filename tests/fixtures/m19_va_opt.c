#define LOG(fmt, ...) fmt __VA_OPT__(, __VA_ARGS__)
int main(void) {
    int x = (LOG(1, 2));
    if (x != 2) return 1;
    int y = (LOG(3));
    if (y != 3) return 2;
    return 0;
}
