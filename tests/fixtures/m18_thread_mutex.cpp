// m18_thread_mutex.cpp - thread / mutex smoke test
// 不真启动线程 (Linux 下 pthread_create 可用, 但需要 host pthread
// link). 仅验证类型 + 头可解析.

#include <thread>
#include <mutex>

int test_thread_mutex_smoke() {
    std::mutex m;
    m.lock();
    m.unlock();
    if (!m.try_lock()) return 1;
    m.unlock();
    return 0;
}

int main() {
    return test_thread_mutex_smoke();
}
