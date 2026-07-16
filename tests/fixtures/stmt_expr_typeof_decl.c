int main(void) {
    return ({ typeof(100) x_ = 100; typeof(x_) q = x_ / 10; q; });
}
