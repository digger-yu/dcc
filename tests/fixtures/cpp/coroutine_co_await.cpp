// Coroutine co_await runtime test
// Verifies: co_await in coroutine state machine
// compile+link+run: dcc -c + host gcc link + execute

int awaitable() {
    co_await 1;
    co_await 2;
    co_return 42;
}

int main() {
    if (awaitable() != 1) return 1;
    if (awaitable() != 2) return 2;
    if (awaitable() != 42) return 3;
    return 0;
}
