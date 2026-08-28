/* Test: C23 char8_t type */
char8_t c1 = 'A';

_Static_assert(sizeof(char8_t) == 1, "char8_t is 1 byte");
_Static_assert(_Alignof(char8_t) == 1, "char8_t align is 1");

int main(void) {
    char8_t x = 'Z';
    return (int)x;
}
