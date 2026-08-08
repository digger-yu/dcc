/* E0014: output constraint missing '=' or '+' prefix */
int main(void) {
    int x = 0;
    asm (
        "mov %0, $1"
        : "r" (x)
    );
    return x;
}
