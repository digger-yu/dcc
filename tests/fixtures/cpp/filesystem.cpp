// m18_filesystem.cpp - std::filesystem 头 smoke test

#include <filesystem>

int test_filesystem_path() {
    std::filesystem::path p("/tmp");
    std::filesystem::path q = p / "dcc";
    if (!(p == "/tmp")) return 1;
    if (q.size() == 0) return 2;
    /* std::filesystem::exists 仅运行时检查, 不强制 */
    return 0;
}

int main() {
    return test_filesystem_path();
}
