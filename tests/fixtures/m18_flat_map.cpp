// m18_flat_map.cpp - std::flat_map 头 smoke test (C++23)

#include <flat_map>

int test_flat_map_basic() {
    std::flat_map_ii m;
    if (!m.empty()) return 1;

    m.insert(3, 30);
    m.insert(1, 10);
    m.insert(2, 20);

    if (m.size() != 3) return 2;
    if (m.at(1) != 10) return 3;
    if (m.at(2) != 20) return 4;
    if (m.at(3) != 30) return 5;

    /* 二分查找保持排序: 键 1, 2, 3 */
    if (m.key_at(0) != 1) return 6;
    if (m.key_at(1) != 2) return 7;
    if (m.key_at(2) != 3) return 8;

    if (!m.contains(2)) return 9;
    if (m.contains(99)) return 10;

    if (!m.erase(2)) return 11;
    if (m.contains(2)) return 12;
    if (m.size() != 2) return 13;

    return 0;
}

int main() {
    return test_flat_map_basic();
}
