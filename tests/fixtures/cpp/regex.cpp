// m18_regex.cpp - std::regex 简化版头 smoke test
// 支持: 字面 / . / [..] / * / + / ? / | / ().

#include <regex>

int test_regex_literal() {
    std::regex r("hello");
    if (!r.match_ok()) return 1;
    if (!r.match("hello")) return 2;
    if (r.match("world")) return 3;
    return 0;
}

int test_regex_dot() {
    std::regex r("h.llo");
    if (!r.match("hello")) return 1;
    if (!r.match("hxllo")) return 2;
    if (r.match("hello!")) return 3;
    return 0;
}

int test_regex_star() {
    std::regex r("ab*c");
    if (!r.match("ac")) return 1;
    if (!r.match("abc")) return 2;
    if (!r.match("abbbbc")) return 3;
    if (r.match("axc")) return 4;
    return 0;
}

int test_regex_class() {
    std::regex r("[abc]+");
    if (!r.match("abc")) return 1;
    if (!r.match("ccc")) return 2;
    if (r.match("xyz")) return 3;
    return 0;
}

int test_regex_alt() {
    std::regex r("cat|dog");
    if (!r.match("cat")) return 1;
    if (!r.match("dog")) return 2;
    if (r.match("bird")) return 3;
    return 0;
}

int test_regex_group() {
    std::regex r("(ab)+");
    if (!r.match("ab")) return 1;
    if (!r.match("abab")) return 2;
    if (r.match("a")) return 3;
    return 0;
}

int main() {
    int r = 0;
    r += test_regex_literal();
    r += test_regex_dot();
    r += test_regex_star();
    r += test_regex_class();
    r += test_regex_alt();
    r += test_regex_group();
    return r;
}
