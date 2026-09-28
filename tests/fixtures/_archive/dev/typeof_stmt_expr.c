int main(void) {
    return ({
        __typeof__(1) x = (__typeof__(1))(2);
        x;
    });
}
