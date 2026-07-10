struct flex {
    int len;
    char data[];
};

int main(void) {
    /* sizeof 应只包含 len 的大小 (4字节) */
    if (sizeof(struct flex) != 4) return 1;
    return 0;
}
