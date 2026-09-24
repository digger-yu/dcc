// src/codegen/x86_64/special/mod.rs
// Special features handling for x86_64 backend
// __int128 / vector, x87 floating point, sanitizer, TLS support
//
// 各子模块自 batch B 起为真实生产路径（mod.rs 逐字搬迁，非影子实现）。

pub mod int128;
// batch C/D/E 已全部接线投产：x87 / sanitizer / tls 子模块均为 mod.rs 逐字搬迁的
// 生产实现（非影子），`#[allow(unused_imports)]` 仅为兼容后续逐模块引用。
#[allow(unused_imports)]
pub mod sanitizer;
#[allow(unused_imports)]
pub mod tls;
#[allow(unused_imports)]
pub mod x87_floating_point;
