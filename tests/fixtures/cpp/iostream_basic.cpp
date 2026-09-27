// C++98 <iostream>: clog / cerr 流 + endl / ws manipulator
//
// 覆盖点:
//   1. std::clog / std::cerr / std::cout / std::cin 四个标准流都可取用;
//   2. `<<` 链式调用 (含 const char* / char / int / long / double / bool),
//      每一步都必须返回 `ostream&` 以便继续串联;
//   3. `<< std::endl` / `<< std::flush` 走 **manipulator 重载**
//      (头文件里没有 `operator<<(ostream&(*)(ostream&))` 时这一步无法通过);
//   4. `>> std::ws` 同理走 `operator>>(istream&(*)(istream&))`;
//   5. flush() / put() / write() / getline() / eof() 成员存在且可调用.
//
// 方言差异: dcc 的 <iostream> 是 stub, 标准流以函数形式提供 (`std::clog()`),
// 而 libstdc++ 是全局对象 (`std::clog`). 用 __DCC__ 宏切换, 使同一份夹具
// 既能被 dcc 编译, 也能被 g++ -std=c++98 编译链接运行, 用于交叉验证语义.
#include <iostream>

#ifdef __DCC__
#define DCC_COUT std::cout()
#define DCC_CLOG std::clog()
#define DCC_CERR std::cerr()
#define DCC_CIN  std::cin()
#else
#define DCC_COUT std::cout
#define DCC_CLOG std::clog
#define DCC_CERR std::cerr
#define DCC_CIN  std::cin
#endif

// clog 链式输出 + endl/flush manipulator; 返回链尾流的字符计数以迫使求值.
int clog_chain() {
    DCC_CLOG << "dcc" << '!' << 42 << 7L << 3.5 << true << std::endl;
    DCC_CLOG << 1 << std::flush;
    DCC_CLOG.put('x');
    DCC_CLOG.write("yz", 2);
    DCC_CLOG.flush();
    return 0;
}

// cerr 与 clog 是两个不同的流对象
int cerr_is_distinct_from_clog() {
    void* a = (void*)&DCC_CERR;
    void* b = (void*)&DCC_CLOG;
    return a == b;
}

// cin + ws manipulator: 无输入时 (stdin 为 /dev/null) 只跳过空白, v 保持 0.
int cin_with_ws() {
    int v = 0;
    DCC_CIN >> std::ws >> v;
    char c = 0;
    DCC_CIN >> c;
    if (DCC_CIN.eof()) v = 0;
    return v;
}

// 读入缓冲: getline / get / peek / ignore 成员必须存在且类型正确.
// stdin 为 /dev/null 时全部在 EOF 上返回, 只要求调用能通过类型检查.
int istream_members() {
    char buf[8];
    buf[0] = 0;
    DCC_CIN.getline(buf, 8);
    DCC_CIN.ignore();
    DCC_CIN.peek();
    DCC_CIN.get();
    return (int)buf[0];
}

int main() {
    // cout 只验证"可取用且与其它流互不相同": 不向 stdout 写任何内容,
    // 这样 dcc(stub 头, 写入无效果) 与 g++(真实 libstdc++) 的 stdout 可逐字节对照.
    void* co = (void*)&DCC_COUT;
    void* cl = (void*)&DCC_CLOG;
    void* ci = (void*)&DCC_CIN;
    if (co == 0 || cl == 0 || ci == 0) return 5;
    if (co == cl || co == ci || cl == ci) return 6;
    if (clog_chain() != 0) return 1;
    if (cerr_is_distinct_from_clog() != 0) return 2;
    if (cin_with_ws() != 0) return 3;
    if (istream_members() != 0) return 4;
    return 0;
}
