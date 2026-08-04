// src/mmap_io.rs - ADR-006: Unix mmap 零拷贝读源，Windows 回退 read
// 第一性原理: 大文件预处理不必先复制到堆；映射只读私有页即可。

use std::io::{self, Read};
use std::path::Path;

/// 映射或读入的源文件；Drop 时自动 munmap。
pub struct MappedInput {
    storage: Storage,
}

enum Storage {
    Owned(Vec<u8>),
    #[cfg(unix)]
    Mapped(MmapRegion),
}

#[cfg(unix)]
struct MmapRegion {
    ptr: *const u8,
    len: usize,
}

#[cfg(unix)]
impl Drop for MmapRegion {
    fn drop(&mut self) {
        if !self.ptr.is_null() && self.len > 0 {
            unsafe {
                platform_munmap(self.ptr as *mut u8, self.len);
            }
        }
    }
}

impl MappedInput {
    pub fn open(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            if let Ok(mapped) = try_mmap_file(path) {
                return Ok(mapped);
            }
        }
        Ok(Self {
            storage: Storage::Owned(std::fs::read(path)?),
        })
    }

    pub fn from_stdin(mut reader: impl Read) -> io::Result<Self> {
        let mut data = Vec::new();
        reader.read_to_end(&mut data)?;
        Ok(Self {
            storage: Storage::Owned(data),
        })
    }

    pub fn as_slice(&self) -> &[u8] {
        match &self.storage {
            Storage::Owned(v) => v,
            #[cfg(unix)]
            Storage::Mapped(m) => unsafe { std::slice::from_raw_parts(m.ptr, m.len) },
        }
    }
}

#[cfg(unix)]
fn try_mmap_file(path: &Path) -> io::Result<MappedInput> {
    use std::fs::File;
    use std::os::unix::io::AsRawFd;

    let file = File::open(path)?;
    let len = file.metadata()?.len() as usize;
    if len == 0 {
        return Ok(MappedInput {
            storage: Storage::Owned(Vec::new()),
        });
    }
    let fd = file.as_raw_fd();
    let ptr = unsafe { platform_mmap(len, fd) };
    if ptr.is_null() {
        return Err(io::Error::last_os_error());
    }
    std::mem::forget(file);
    Ok(MappedInput {
        storage: Storage::Mapped(MmapRegion {
            ptr: ptr as *const u8,
            len,
        }),
    })
}

#[cfg(all(unix, target_os = "linux", target_arch = "x86_64"))]
unsafe fn platform_mmap(len: usize, fd: i32) -> *mut u8 {
    let ret: i64;
    std::arch::asm!(
        "syscall",
        in("rax") 9i64,
        in("rdi") 0i64,
        in("rsi") len as i64,
        in("rdx") 1i64,
        in("r10") 2i64,
        in("r8") fd as i64,
        in("r9") 0i64,
        lateout("rax") ret,
        options(nostack),
    );
    if ret < 0 {
        std::ptr::null_mut()
    } else {
        ret as *mut u8
    }
}

#[cfg(all(unix, target_os = "linux", target_arch = "aarch64"))]
unsafe fn platform_mmap(len: usize, fd: i32) -> *mut u8 {
    let ret: i64;
    std::arch::asm!(
        "svc 0",
        in("x8") 222i64,
        in("x0") 0i64,
        in("x1") len as i64,
        in("x2") 1i64,
        in("x3") 2i64,
        in("x4") fd as i64,
        in("x5") 0i64,
        lateout("x0") ret,
        options(nostack),
    );
    if ret < 0 {
        std::ptr::null_mut()
    } else {
        ret as *mut u8
    }
}

#[cfg(all(unix, target_os = "linux", target_arch = "x86_64"))]
unsafe fn platform_munmap(ptr: *mut u8, len: usize) {
    let _: i64;
    std::arch::asm!(
        "syscall",
        in("rax") 11i64,
        in("rdi") ptr as i64,
        in("rsi") len as i64,
        lateout("rax") _,
        options(nostack),
    );
}

#[cfg(all(unix, target_os = "linux", target_arch = "aarch64"))]
unsafe fn platform_munmap(ptr: *mut u8, len: usize) {
    let _: i64;
    std::arch::asm!(
        "svc 0",
        in("x8") 215i64,
        in("x0") ptr as i64,
        in("x1") len as i64,
        lateout("x0") _,
        options(nostack),
    );
}

#[cfg(all(unix, not(target_os = "linux")))]
unsafe fn platform_mmap(len: usize, fd: i32) -> *mut u8 {
    extern "C" {
        fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    }
    mmap(std::ptr::null_mut(), len, 1, 2, fd, 0)
}

#[cfg(all(unix, not(target_os = "linux")))]
unsafe fn platform_munmap(ptr: *mut u8, len: usize) {
    extern "C" {
        fn munmap(addr: *mut u8, len: usize) -> i32;
    }
    let _ = munmap(ptr, len);
}

#[cfg(all(
    unix,
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
unsafe fn platform_mmap(len: usize, fd: i32) -> *mut u8 {
    extern "C" {
        fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    }
    mmap(std::ptr::null_mut(), len, 1, 2, fd, 0)
}

#[cfg(all(
    unix,
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
unsafe fn platform_munmap(ptr: *mut u8, len: usize) {
    extern "C" {
        fn munmap(addr: *mut u8, len: usize) -> i32;
    }
    let _ = munmap(ptr, len);
}
