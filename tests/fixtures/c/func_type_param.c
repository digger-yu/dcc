// C89 函数类型参数: 参数声明为函数类型时按 C11 6.7.6.3p8
// 调整为指向函数的指针.
// 内核 include/net/ipv6.h 的 ip6_append_data 原型:
//   int ip6_append_data(struct sock *sk,
//                       int getfrag(void *from, char *to, int offset, int len,
//                                   int odd, struct sk_buff *skb),
//                       ...);
// 修复前 parse_params 遇到参数名后的 '(' 直接报 E0001 语法错误.

typedef unsigned long size_t;

struct sk_buff;
struct sock;
struct ipcm6_cookie;
struct flowi6;
struct rt6_info;

int ip6_append_data(struct sock *sk,
                    int getfrag(void *from, char *to, int offset, int len,
                                int odd, struct sk_buff *skb),
                    void *from, size_t length, int transhdrlen,
                    struct ipcm6_cookie *ipc6, struct flowi6 *fl6,
                    struct rt6_info *rt, unsigned int flags);
