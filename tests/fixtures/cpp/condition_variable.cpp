// m18_condition_variable.cpp - condition_variable 头 smoke test

#include <condition_variable>
#include <mutex>

int test_cv() {
    std::mutex m;
    std::condition_variable cv;
    m.lock();
    m.unlock();
    cv.notify_one();
    cv.notify_all();
    return 0;
}

int main() {
    return test_cv();
}
