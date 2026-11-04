//! 编译器**内部**字符串键表用的快速哈希（FNV-1a 64）。
//!
//! 为什么不用 `std` 的默认 `RandomState`：它走 SipHash-1-3，为「对抗恶意构造的键」设计。
//! 而预处理器在**每个标识符一次** `macros.get(name)` 的量级上跑（一个内核 TU 展开 14 万行、
//! 几百万次查表），`perf` 在冷读 `net/core/skbuff.c` 上直接量到 `core::hash::sip::Hasher::write`
//! 占 24.8%、`memcmp` 9.1% —— 那是哈希本身，不是查表逻辑。换成一次字节循环的 FNV-1a 是
//! 编译器行业常规做法（clang/gcc 的内部表同理）。
//!
//! **边界**（不许越出去用）：
//!   * 只给「同一台机器、自己喂自己源码」的进程内表用。抗碰撞性正是 SipHash 的价值所在：
//!     若某天这些表要吃**不受信**输入（比如把别人的 .c 当服务编译），要么回到 `RandomState`，
//!     要么加输入长度/键数上限。
//!   * **跨进程或落盘的键**（缓存文件名、增量指纹）继续用 `std` 的 `DefaultHasher`，
//!     那些地方要的是分布质量与稳定性，不是吞吐。
use std::hash::{BuildHasher, Hasher};

/// FNV-1a 64 位状态。
#[derive(Debug, Clone)]
pub struct FastStrHasher(u64);

/// FNV-1a 的 offset basis（`0xcbf29ce484222325`）与 prime（`2**40 + 1`）。
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

impl Hasher for FastStrHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(FNV_PRIME);
        }
    }
    fn write_u8(&mut self, i: u8) {
        self.0 ^= i as u64;
        self.0 = self.0.wrapping_mul(FNV_PRIME);
    }
    fn write_u64(&mut self, i: u64) {
        self.write(&i.to_le_bytes());
    }
    fn write_usize(&mut self, i: usize) {
        self.write(&(i as u64).to_le_bytes());
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FastStrBuildHasher;

impl BuildHasher for FastStrBuildHasher {
    type Hasher = FastStrHasher;
    fn build_hasher(&self) -> FastStrHasher {
        FastStrHasher(FNV_OFFSET)
    }
}

/// 进程内的「字符串 → 值」表。
pub type FastStrMap<V> = std::collections::HashMap<String, V, FastStrBuildHasher>;
/// 进程内的字符串集合。
pub type FastStrSet = std::collections::HashSet<String, FastStrBuildHasher>;
