// tests/fixtures/static_assert_msg.c
// _Static_assert 接受 (cond) 或 (cond, "msg")；
// 失败时错误消息包含 "evaluates to: <cond-text>"。

_Static_assert(sizeof(int) == 4, "int must be 4 bytes");
_Static_assert(2 + 2 == 4);

struct S {
    int x;
};
_Static_assert(__builtin_offsetof(struct S, x) == 0, "first field offset is 0");

int main(void) {
    return 0;
}
