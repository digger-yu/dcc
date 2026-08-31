// C++11: using type alias (equivalent to typedef)
using int_alias = int;
using long_alias = long;

struct Point {
    int x;
    int y;
};

using point_alias = Point;

int main() {
    int_alias a = 42;
    long_alias b = 100L;
    point_alias p;
    p.x = a;
    p.y = (int)b;
    return p.x - 42;
}
