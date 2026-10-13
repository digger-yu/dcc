// 全局静态初始化器的链接期地址常量 (E1006 回归):
// 1) 数组成员隐式数组退化: init_uts_ns.name.machine (char[65]) 直接作值,
//    等价 &init_uts_ns.name.machine[0] (kernel/utsname_sysctl.c)
// 2) &member_chain[i]: &tracepoint_srcu_data.srcu_ctrs[0] (kernel/tracepoint.c)
// 3) (&x)->field: cpumask_bits(&mask) 宏展开形式 (kernel/watchdog.c)

struct new_utsname {
    char machine[65];
};
struct uts_namespace {
    struct new_utsname name;
};
struct uts_namespace init_uts_ns;

struct ctl_table {
    void *data;
};
static const struct ctl_table uts_kern_table[] = {
    {
        .data = init_uts_ns.name.machine,
    },
};

struct srcu_struct {
    unsigned long srcu_ctrs[2];
};
struct srcu_struct tracepoint_srcu_srcu_data;
static void *srcu_ctrs_ptr = &tracepoint_srcu_srcu_data.srcu_ctrs[0];

struct cpumask {
    unsigned long bits[2];
};
struct cpumask watchdog_cpumask;
static unsigned long *watchdog_cpumask_bits = (&watchdog_cpumask)->bits;
