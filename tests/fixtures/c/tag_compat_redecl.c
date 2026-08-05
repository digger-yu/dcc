// 同标签 struct/union 指针的重声明兼容: C11 6.2.5p22 规定同标签的
// struct/union 无论是否完整都是同一类型.
// 内核 include/linux/sched.h 先声明:
//   extern struct pid *cad_pid;    // 此时 struct pid 标签尚不完整
// 随后 kernel/reboot.c 完整定义 struct pid 并定义 cad_pid:
//   struct pid *cad_pid;
// 修复前 check_var_redecl 的"空->非空"例外只匹配裸 Struct/Union,
// Ptr 包装后失败, 误报 E0007.

extern struct pid *cad_pid;

struct pid {
    int x;
};

struct pid *cad_pid;
