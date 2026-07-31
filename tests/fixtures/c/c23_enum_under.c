// C23: enum 底层类型 `enum E : type` (大小/取值范围/前向声明)
#include <stdio.h>

enum E8 : unsigned char { A8 = 0, B8 = 255 };
enum E16 : unsigned short { A16 = 0, B16 = 65535 };
enum E32 : int { A32 = -1, B32 = 2147483647 };
enum EU32 : unsigned int { AU32 = 0, BU32 = 4294967295u };
enum EBool : bool { AB = false, BB = true };
enum EPlain { AP = 0, BP = 5 };          // 默认 int
enum E : long;                            // 前向声明带底层类型

// packed 属性与底层类型共存
enum __attribute__((packed)) EP { P1 = 1, P2 = 2 };

int main(void) {
    // 底层类型决定大小
    if (sizeof(enum E8) != 1) return 1;
    if (sizeof(enum E16) != 2) return 2;
    if (sizeof(enum E32) != 4) return 3;
    if (sizeof(enum EU32) != 4) return 4;
    if (sizeof(enum EBool) != 1) return 5;
    if (sizeof(enum EPlain) != 4) return 6;
    if (sizeof(enum EP) != 1) return 7;

    // 值可完整容纳
    enum E8 e8 = B8;
    enum E16 e16 = B16;
    enum E32 e32 = B32;
    enum EU32 eu = BU32;
    if (e8 != 255) return 8;
    if (e16 != 65535) return 9;
    if (e32 != 2147483647) return 10;
    if (eu != 4294967295u) return 11;
    if (AB != 0 || BB != 1) return 12;

    // C23: 枚举常量类型 = 枚举底层类型 (E8 为 unsigned char; GCC 实测 sizeof==1)
    if (sizeof(A8) != 1) return 13;

    // bool 底层类型的枚举常量: 类型为 bool, 提升后参与运算
    if (sizeof(AB) != 1) return 15;

    // packed 枚举可用 (1 字节)
    enum EP ep = P2;
    if (ep != 2) return 14;

    printf("OK sizes=%d,%d,%d,%d,%d,%d,%d e8=%d e16=%d e32=%d eu=%u ep=%d\n",
           (int)sizeof(enum E8), (int)sizeof(enum E16), (int)sizeof(enum E32),
           (int)sizeof(enum EU32), (int)sizeof(enum EBool), (int)sizeof(enum EPlain),
           (int)sizeof(enum EP), e8, e16, e32, eu, ep);
    return 0;
}
