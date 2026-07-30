int warn(void) {
    return ({ __auto_type __flags = (1 << 0); __flags; });
}

int main(void) { return warn(); }
