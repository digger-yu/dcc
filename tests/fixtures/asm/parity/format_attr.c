__attribute__((format(printf, 1, 2)))
void log_msg(const char *fmt, ...);

int main(void) {
    log_msg("value=%d", 42);
    return 0;
}
