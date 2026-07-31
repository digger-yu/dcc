// M17.0: friend 关键字被解析层接受 (lower 层接受 friend 声明)
// 覆盖: friend 关键字解析; 实际访问权限检查在语义层, M17.0 暂不强求
class Secret {
public:
  int code;
  Secret(int c) : code(c) {}
  friend int peek(Secret& s);
};
int peek(Secret& s) {
  return (&s)->code;
}
int test_friend() {
  Secret s(99);
  return peek(s);
}
