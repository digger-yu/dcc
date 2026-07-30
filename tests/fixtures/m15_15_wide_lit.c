#include <wchar.h>
int main(void) {
    wchar_t a = L'a';
    wchar_t b = L'\n';
    wchar_t c = L'Z';
    /* ASCII 字符值正确; 换行符 0x0a */
    return (a == 'a' && b == '\n' && c == 'Z') ? 0 : 1;
}
