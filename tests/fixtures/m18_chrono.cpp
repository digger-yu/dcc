// m18_chrono.cpp - std::chrono 头编译 smoke test (M18)
// 验证: duration / time_point / system_clock / steady_clock
// / high_resolution_clock 编译通过 + 运行时调用.
// chrono 头不暴露 operator overload, 改用 add / sub / add_dur
// / sub_dur / diff / count_* 方法. 不验证时间值精度.

#include <chrono>

int test_chrono_basic() {
    std::duration ms(100);
    std::duration s(2000000000LL);  // 2 seconds in ns
    if (ms.count_nanos() != 100) return 1;
    if (s.count_secs() != 2) return 2;

    std::duration sum = ms.add(&s);
    if (sum.count_nanos() != 2000000100) return 3;

    std::duration diff = s.sub(&ms);
    if (diff.count_nanos() != 1999999900) return 4;

    if (!ms.lt(&s)) return 5;
    if (s.lt(&ms)) return 6;
    if (!ms.eq(&ms)) return 7;

    std::time_point tp = std::time_point::now();
    std::time_point tp2 = tp.add_dur(&ms);
    std::duration d2 = tp2.diff(&tp);
    if (d2.count_nanos() != 100) return 8;
    if (!tp2.eq(&tp.add_dur(&ms))) return 9;

    dccpp_time_point* sys_now = std::system_clock::now();
    dccpp_time_point* st_now = std::steady_clock::now();
    dccpp_time_point* hr_now = std::high_resolution_clock::now();
    if (sys_now == 0) return 10;
    if (st_now == 0) return 11;
    if (hr_now == 0) return 12;
    dccpp_tp_free(sys_now);
    dccpp_tp_free(st_now);
    dccpp_tp_free(hr_now);

    return 0;
}

int main() {
    return test_chrono_basic();
}
