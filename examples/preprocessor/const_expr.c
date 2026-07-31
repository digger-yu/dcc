int main() {
    int result = 0;
#if defined(__GNUC__) && __GNUC__ >= 10
    result += 1;
#endif
#if (1 ? 2 : 0) == 2
    result += 2;
#endif
#if (0xFF & 0x0F) == 0x0F
    result += 4;
#endif
#if (0xF0 | 0x0F) == 0xFF
    result += 5;
#endif
#if (0xFF ^ 0x0F) == 0xF0
    result += 6;
#endif
#if (1 << 3) == 8
    result += 7;
#endif
#if (16 >> 2) == 4
    result += 8;
#endif
#if 'A' == 65
    result += 9;
#endif
    return result;
}
