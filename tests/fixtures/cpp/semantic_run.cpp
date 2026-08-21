// Semantic run test: enum + type traits + sizeof
// compile+link+run: dcc -c + host gcc link + execute
// Verifies: enum values, scoped enum, sizeof type properties

// Regular enum
enum Color { RED, GREEN = 5, BLUE };

// Scoped enum with explicit underlying type
enum class Scoped : unsigned char { A = 1, B = A + 2 };

// Anonymous enum
enum { ANON_X = 42 };

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

    return 0;
}
