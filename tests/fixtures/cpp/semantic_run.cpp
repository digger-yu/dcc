// Semantic run test: enum + type traits + sizeof
// compile+link+run: dcc -c + host gcc link + execute
//
// 校验的 "type traits" 语义 (全部在运行期可观测, 与 g++ 逐条对照):
//   - is_enum 类: 枚举量可隐式退化为其底层整数并参与整数运算 (int 提升);
//   - underlying_type 类: sizeof(枚举类型) == sizeof(底层类型),
//     unsigned char / short / unsigned int / long long 四种底层类型的
//     尺寸、符号性 (200 不回绕 / -1 不变 65535) 与值域;
//   - enumerator 常量表达式: B = A + 2、隐式递增 (GREEN=5 -> BLUE=6)、
//     首值 0、匿名枚举、枚举量作数组维度;
//   - scoped enum: 必须显式转换, 同作用域不冲突, 关系比较成立.

// Regular enum (底层类型 int)
enum Color { RED, GREEN = 5, BLUE };

// Scoped enum with explicit underlying type
enum class Scoped : unsigned char { A = 1, B = A + 2 };

// Anonymous enum
enum { ANON_X = 42 };

// 显式底层类型: 无符号 8bit / 有符号 16bit / unsigned int / long long
enum Level : unsigned char { LOW = 200, HIGH = 201 };
enum Signs : short { NEG = -1, ZERO = 0, POS = 1 };
enum Mask : unsigned int { M_LO = 0x80000000u, M_HI = 0xFFFFFFFFu };
enum Big : long long { BIG = 5000000000LL };

// 枚举量作数组维度 (ANON_X = 42 个 int) —— 见 main 内的局部数组

// 枚举 -> int 的隐式转换 (is_enum 退化为整数的可执行证明)
int take_int(int v) { return v * 2; }

// 底层类型显式参与运算: 若 underlying_type 记错, 这里的截断/符号扩展会变结果
int underlying_roundtrip() {
    Level lv = LOW;            // unsigned char 存 200: 不截断
    Signs sv = NEG;            // short 存 -1: 不变成 65535
    Mask mv = M_LO;            // unsigned int 存 0x80000000: 不变成负数
    Big bv = BIG;              // long long 存 >32bit: 不溢出
    int acc = 0;
    acc += (int)lv;            // 200
    acc += (int)sv;            // -1
    acc += (mv > 0u) ? 1 : 0;  // unsigned 比较
    acc += (int)(bv - 5000000000LL);
    return acc;                // 200 - 1 + 1 + 0 = 200
}

int main() {
    // Enum value checks
    int c = GREEN;
    if (c != 5) return 1;

    // Scoped enum
    int s = (int)Scoped::B;
    if (s != 3) return 2;  // B == A + 2 == 1 + 2 == 3

    // Anonymous enum
    if (ANON_X != 42) return 3;

    // Implicit enum values
    if (BLUE != 6) return 4;  // follows GREEN=5
    if (RED != 0) return 5;   // first value

    // sizeof checks
    if (sizeof(Scoped) != 1) return 6;  // unsigned char
    if (sizeof(int) != 4) return 7;     // x86_64
    if (sizeof(char) != 1) return 8;

    // underlying_type 决定 sizeof: 未指定 -> int, 指定 -> 该类型
    if (sizeof(Color) != sizeof(int)) return 9;
    if (sizeof(Level) != 1) return 10;
    if (sizeof(Signs) != 2) return 11;
    if (sizeof(Mask) != 4) return 12;
    if (sizeof(Big) != 8) return 13;
    if (sizeof(Scoped::A) != 1) return 14;   // 枚举量的 sizeof 同样是底层类型

    // 枚举量值域 / 符号性
    if (LOW != 200 || HIGH != 201) return 15;
    if (NEG != -1 || POS != 1 || ZERO != 0) return 16;
    if (M_LO != 0x80000000u || M_HI != 0xFFFFFFFFu) return 17;
    if (BIG != 5000000000LL) return 18;

    // is_enum: 隐式退化到 int 后按整数运算 (GREEN + BLUE = 11)
    if (GREEN + BLUE != 11) return 19;
    if (take_int(GREEN) != 10) return 20;
    if (take_int(BLUE) != 12) return 21;

    // 底层类型显式往返
    if (underlying_roundtrip() != 200) return 22;

    // scoped enum 的关系比较与显式转换
    if (!(Scoped::A < Scoped::B)) return 23;
    if ((int)Scoped::A + (int)Scoped::B != 4) return 24;
    if ((Scoped)3 != Scoped::B) return 25;

    // 枚举量作数组维度: sizeof(数组) == 42 * 4, 且可按普通数组读写
    int g_table[ANON_X];
    if (sizeof(g_table) != 42 * 4) return 26;
    g_table[GREEN] = BLUE;
    g_table[RED] = ANON_X;
    if (g_table[5] != 6) return 27;
    if (g_table[0] != 42) return 28;

    // 枚举变量参与算术时必须先退化为整数 (C++ 不允许 int -> 枚举 隐式转换)
    Color it = RED;
    int mixed = (int)it + GREEN;
    if (mixed != 5) return 29;
    if ((int)BLUE + 1 != 7) return 30;
    if (take_int((int)it + BLUE) != 12) return 31;

    return 0;
}
