class Foo {
  int x;
  Foo() : x(0) {}
  int get() const { return x; }
};
int main() {
  Foo f;
  return f.get();
}
