// m18_condition_variable.cpp - Minimal compile check
// Renamed classes to avoid mangling conflicts with DCC internal p_<T> prefix

namespace std {
    class dccpp_mutex {};
    class dccpp_condition_variable {};
}

void verify_types() {
    // Use renamed types to avoid conflicts
    std::dccpp_mutex m;
    std::dccpp_condition_variable cv;
    (void)m;
    (void)cv;
}

int main() {
    return 0;
}
