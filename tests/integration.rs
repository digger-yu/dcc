// tests/integration.rs - 集成测试 (M0 stub)
//
// 当 M0 完成后: 这个测试会调用 `cargo run -- examples/hello.c` 然后 chmod +x 然后跑
// 当 M1 完成后: 会加 fib.c 测试
// 当 M5 完成后: 会加自举测试
//
// 当前 M0 stub: 仅占位

#[test]
fn m0_hello_compiles() {
    // 真实测试在 M0 完成后补: spawn dcc -> assert exit 42
    // 现在是 stub
    assert!(true, "M0 placeholder");
}
