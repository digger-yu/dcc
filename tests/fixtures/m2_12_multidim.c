struct xa_node {
    unsigned long tags[3][((((1UL << (0 ? 4 : 6))) + (64) - 1) / (64))];
};

int main(void) { return 0; }
