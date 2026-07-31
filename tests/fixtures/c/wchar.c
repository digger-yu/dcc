// wchar_t wcs* / wmem* libc forwarding fixture (Section 11 P0.1).
// Each step returns a non-zero value on failure; main() returns 0 on success.
// wchar_t 在 Linux 为 4 字节 (int).
// 注: 故意只用一个数组, 因为 dcc 当前的多数组栈变量分配有重叠 bug
// (Section 9 follow-up), 与本测试无关.
#include <wchar.h>

int main(void) {
    wchar_t buf[32];
    wchar_t *p;
    int a;

    // 初始化: buf = "hello\0...".
    buf[0] = L'h'; buf[1] = L'e'; buf[2] = L'l';
    buf[3] = L'l'; buf[4] = L'o'; buf[5] = L'\0';
    for (int i = 6; i < 32; i++) buf[i] = L'\0';

    // wcslen: "hello" = 5 字符.
    a = (int)wcslen(buf);
    if (a != 5) return 1;

    // wcschr: 找 'e' → &buf[1].
    p = wcschr(buf, L'e');
    if (p != buf + 1) return 2;

    // wcsrchr: 找 'l' → &buf[3] (最后一次出现).
    p = wcsrchr(buf, L'l');
    if (p != buf + 3) return 3;

    // wmemcpy: 把 buf[0..5] (含 NUL) 拷到 buf[10..15].
    wmemcpy(buf + 10, buf, 6);
    a = (int)wcslen(buf + 10);
    if (a != 5) return 4;

    // wmemcmp: buf[0..5] vs buf[10..15] → 0.
    a = wmemcmp(buf, buf + 10, 5);
    if (a != 0) return 5;

    // wmemset: 头 3 个 wchar_t 设为 'X'.
    wmemset(buf, L'X', 3);
    if ((int)buf[0] != (int)L'X') return 6;
    if ((int)buf[1] != (int)L'X') return 7;
    if ((int)buf[2] != (int)L'X') return 8;
    if ((int)buf[3] != (int)L'l') return 9;  // 未触及

    // wmemmove: 用 wmemmove 把 buf[10..15] 拷到 buf[0..5].
    wmemmove(buf, buf + 10, 6);
    if ((int)buf[0] != (int)L'h') return 10;
    if ((int)buf[4] != (int)L'o') return 11;
    if ((int)buf[5] != L'\0') return 12;

    // wcscmp: buf (现在 "hello\0") vs buf + 10 ("hello\0") → 0.
    a = wcscmp(buf, buf + 10);
    if (a != 0) return 13;

    // wcsncmp: 前 5 字符相同 → 0.
    a = wcsncmp(buf, buf + 10, 5);
    if (a != 0) return 14;

    // wcscat: 追加 "!" 到 buf (源在 buf+10, 临时改为 "!" + NUL).
    buf[10] = L'!';
    buf[11] = L'\0';
    wcscat(buf, buf + 10);
    a = (int)wcslen(buf);
    if (a != 6) return 15;
    if ((int)buf[5] != (int)L'!') return 16;

    return 0;
}
