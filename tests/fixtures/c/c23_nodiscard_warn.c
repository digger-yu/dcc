/* Test: [[nodiscard]] warning when return value is discarded */
[[nodiscard]] int important_func(void) {
    return 100;
}

int main(void) {
    /* This should emit W0401: ignoring return value of nodiscard function */
    important_func();
    return 0;
}
