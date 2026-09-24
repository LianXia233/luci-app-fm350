/*
 * atproxy —— F22 AP 侧 EIF 语义代理（只读）
 *
 * 订阅 MIPC EIF_IND (msg id 0x4205，与 mtk_netagent 同源)，把 MD 上报的
 * 内部接口事件降维成不含 NetIF id、只读、语义明确的 +GT* 形态，写到
 * /dev/ttyGS1（主机看到的 /dev/ttyUSB1，luci-app-fm350 的 AT 口）。
 *
 * 设计依据：outputs/FM350-F22-AP侧AT命令透传可行性.md §3。
 * 铁律：
 *   - 只订阅（IND），绝不向 MD 发任何 AT+EIF= 写命令（写方向会抢
 *     mtk_netagent 的状态机，no_ra 回调甚至可能去激活数据呼叫）。
 *   - 不输出 MD 内部 trans_intf_id，主机无从误用。
 *   - 全部输出为告知性 URC，主机不依赖它们也能正常工作。
 *
 * 依赖（全部为 F22 root.squashfs 自带）：
 *   libmipc_msg.so / libmipc_api.so   MIPC 客户端库（与 atcid/netagent 同款）
 *   musl libc.so                      OpenWrt 19.07 自带
 * 运行方式：procd 管理（见 files/atproxy.init），respawn。
 *
 * 编译：aarch64 musl 交叉工具链，见 Makefile。
 */

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <syslog.h>
#include <termios.h>
#include <unistd.h>

/* ------------------------------------------------------------------ */
/* libmipc_msg.so 的最小 ABI（自 mtk_netagent / fibocom_cmd 反汇编恢复） */

/* mipc_msg_register_ind_api(0x10, msg_id, cb, NULL, NULL)
 * —— netagent 全部指示注册均为 w0=0x10；回调原型 void cb(void *msg)。 */
extern int mipc_init(const char *client_name);
extern void mipc_deinit(void);
extern int mipc_msg_register_ind_api(int arg1, uint16_t msg_id,
                                     void (*cb)(void *msg),
                                     void *aux1, void *aux2);
/* 取 TLV 值指针：netagent 调用形态为 (msg, tag, 0)。
 * tag 以 int16_t 语义传参 —— netagent 反汇编实证：0x100..0x10d 走 MOVZ
 * 正值加载，0x8101..0x810e 一律 MOVN 符号扩展加载（w1=0xFFFF810C）。
 * 本处常量全部按 netagent 同形书写，保证与官方消费者字节级一致。 */
extern void *mipc_msg_get_val_ptr(void *msg, int tag, int arg3);

/* EIF_IND 消息号：netagent 以 0x4205/0x4206 成对注册，其中 0x4205 的
 * 回调提取 9+ 个 TLV（含 %02x 地址处理）且下游日志即 [MEH] EIF_IND *；
 * 0x4206 回调仅取 2 个小字段，为另一简单指示。 */
#define MIPC_IND_EIF 0x4205

/* EIF_IND TLV 标签（自 onEifIndCallback 工作函数 0x407c74 起的反汇编恢复）：
 *   0x100 u32  transid（MD 侧事务号，仅日志用，不外发）
 *   0x101 u8   事件类型索引（netagent 用它查 u16 表得内部事件码 101/203/105）
 *   0x102 u32  cause
 *   0x103 u32  mtu（mtu 事件分支写 cmd_obj 偏移 16 的就是它）
 *   0x104 u8   v4 地址计数
 *   0x106 u8   v6 地址计数
 *   0x107 asciiz reason 动词（"ifst"/"ipadd"/"ipdel"/"ra"/"mtu"/"ho" ...，
 *              与 MD 侧 AT+EIF=%u,"<reason>",... 的动词同源）
 *   0x810c     v4 地址块（netagent @0x404870 以 0xFFFF810C 取出）
 *   0x810e     v6 地址块（netagent @0x40488c 以 0xFFFF810E 取出）
 * 地址块条目（netagent @0x4049c8/@0x404904 双路实证）：
 *   stride 0x14 = 20 字节；地址起始偏移 4 —— V4 取 entry[4..7]
 *   （sprintf("%u.%u.%u.%u", e[4],e[5],e[6],e[7])），V6 取 entry[4..19]。
 *   entry[0..3] 头部语义 netagent 未使用，不外发。 */
#define TLV_TRANSID   0x100
#define TLV_EVTTYPE   0x101
#define TLV_CAUSE     0x102
#define TLV_MTU       0x103
#define TLV_V4CNT     0x104
#define TLV_V6CNT     0x106
#define TLV_REASON    0x107
#define TLV_V4ADDR    ((int)(int16_t)0x810c) /* 0xFFFF810C，同 netagent */
#define TLV_V6ADDR    ((int)(int16_t)0x810e) /* 0xFFFF810E，同 netagent */

#define EIF_ADDR_ENT_SZ 20   /* 地址块条目 stride（反汇编实证） */
#define EIF_ADDR_OFF    4    /* 地址起始字节偏移 */
#define EIF_ADDR_MAX    16   /* 单事件最多外发地址数（防异常计数刷屏） */

/* URC 注入口。ttyGS1 对主机侧即 /dev/ttyUSB1（luci-app-fm350 的 AT 口）；
 * atcid 是同一 tty 的另一个写者，行级写 <512B 由 tty 层保证不撕裂，
 * AT 协议本就允许 URC 随时插行。 */
#define TTY_OUT       "/dev/ttyGS1"

/* ------------------------------------------------------------------ */

static volatile sig_atomic_t g_run = 1;
static int g_tty = -1;

static void on_signal(int sig)
{
    (void)sig;
    g_run = 0;
}

/* 输出一行 URC（调用方保证以 CRLF 结尾）。tty 打不开时仅记日志，
 * 代理仍然活着 —— URC 是锦上添花，不是生命线。 */
static void emit(const char *line)
{
    if (g_tty >= 0) {
        ssize_t n = write(g_tty, line, strlen(line));
        if (n < 0) {
            /* gadget 串口可能随 USB 重枚举消失，标记失效，由主循环重开 */
            close(g_tty);
            g_tty = -1;
        }
    }
    syslog(LOG_INFO, "emit %s", line);
}

/* 地址块条目 -> 文本。V4 直接按 netagent 同款逐字节 sprintf（大端显示序），
 * V6 用 musl 自带 inet_ntop（无新增依赖）。返回写入长度，失败返回 -1。 */
static int eif_addr_to_str(const unsigned char *ent, int is_v6,
                           char *out, size_t outlen)
{
    const unsigned char *a = ent + EIF_ADDR_OFF;

    if (!is_v6) {
        snprintf(out, outlen, "%u.%u.%u.%u", a[0], a[1], a[2], a[3]);
        return (int)strlen(out);
    }
    if (inet_ntop(AF_INET6, a, out, (socklen_t)outlen) == NULL)
        return -1;
    return (int)strlen(out);
}

/* reason 动词 -> 降维 URC。分类主键是 MD 侧的 reason 词根（F22 MD 域
 * 逆向确认的 AT+EIF set 形态），而不是 netagent 的内部事件码表 ——
 * 后者是版本相关的实现细节，前者才是协议语义。 */
static void on_eif_ind(void *msg)
{
    int transid = -1, cause = -1, mtu = -1, v4 = -1, v6 = -1;
    unsigned char evtype = 0xff;
    char reason[48] = {0};
    char line[128];
    const unsigned char *p;

    if ((p = mipc_msg_get_val_ptr(msg, TLV_TRANSID, 0)) != NULL)
        transid = (int)*(const uint32_t *)p;
    if ((p = mipc_msg_get_val_ptr(msg, TLV_EVTTYPE, 0)) != NULL)
        evtype = p[0];
    if ((p = mipc_msg_get_val_ptr(msg, TLV_CAUSE, 0)) != NULL)
        cause = (int)*(const uint32_t *)p;
    if ((p = mipc_msg_get_val_ptr(msg, TLV_MTU, 0)) != NULL)
        mtu = (int)*(const uint32_t *)p;
    if ((p = mipc_msg_get_val_ptr(msg, TLV_V4CNT, 0)) != NULL)
        v4 = p[0];
    if ((p = mipc_msg_get_val_ptr(msg, TLV_V6CNT, 0)) != NULL)
        v6 = p[0];
    if ((p = mipc_msg_get_val_ptr(msg, TLV_REASON, 0)) != NULL) {
        strncpy(reason, (const char *)p, sizeof(reason) - 1);
        for (size_t i = 0; i < sizeof(reason); i++) {
            if (reason[i] == '\r' || reason[i] == '\n')
                reason[i] = '\0'; /* 防注入：reason 永远单行 */
        }
    }

    syslog(LOG_DEBUG,
           "EIF_IND transid=%d evtype=%u cause=%d mtu=%d v4=%d v6=%d reason=%s",
           transid, evtype, cause, mtu, v4, v6, reason);

    if (strstr(reason, "no_ra_initial") != NULL) {
        emit("+GTNORA: initial\r\n");
        return;
    }
    if (strstr(reason, "no_ra_refresh") != NULL) {
        emit("+GTNORA: refresh\r\n");
        return;
    }
    if (strstr(reason, "mtu") != NULL && mtu >= 0) {
        snprintf(line, sizeof(line), "+GTIFMTU: %d\r\n", mtu);
        emit(line);
        return;
    }
    if (strstr(reason, "ifst") != NULL) {
        /* MD 侧 ifst 事件的 up/down 在 reason 之外的参数里；cause>=0 时
         * 原样透出，方向语义由插件结合接口现实状态判定（见插件侧文档）。 */
        snprintf(line, sizeof(line), "+GTIFST: cause=%d\r\n", cause);
        emit(line);
        return;
    }
    if (strstr(reason, "ipadd") != NULL || strstr(reason, "ipdel") != NULL) {
        /* 汇总行 + 每地址一行。地址行自带完整信息，不依赖与汇总行的
         * 行序归属 —— URC 流被 atcid 插行也不破坏解析。 */
        const unsigned char *blk4 = mipc_msg_get_val_ptr(msg, TLV_V4ADDR, 0);
        const unsigned char *blk6 = mipc_msg_get_val_ptr(msg, TLV_V6ADDR, 0);
        char addr[64];

        snprintf(line, sizeof(line), "+GTIFADDR: %s,cause=%d,v4cnt=%d,v6cnt=%d\r\n",
                 reason, cause, v4 < 0 ? 0 : v4, v6 < 0 ? 0 : v6);
        emit(line);

        int n4 = (v4 < 0 || !blk4) ? 0 : (v4 > EIF_ADDR_MAX ? EIF_ADDR_MAX : v4);
        for (int i = 0; i < n4; i++) {
            if (eif_addr_to_str(blk4 + (size_t)i * EIF_ADDR_ENT_SZ, 0,
                                addr, sizeof(addr)) < 0)
                continue;
            snprintf(line, sizeof(line), "+GTIFADDR4: %s\r\n", addr);
            emit(line);
        }
        int n6 = (v6 < 0 || !blk6) ? 0 : (v6 > EIF_ADDR_MAX ? EIF_ADDR_MAX : v6);
        for (int i = 0; i < n6; i++) {
            if (eif_addr_to_str(blk6 + (size_t)i * EIF_ADDR_ENT_SZ, 1,
                                addr, sizeof(addr)) < 0)
                continue;
            snprintf(line, sizeof(line), "+GTIFADDR6: %s\r\n", addr);
            emit(line);
        }
        /* 计数与块缺失不一致时不猜测：缺块就只发汇总行，插件按兜底路径走 */
        return;
    }
    if (reason[0] != '\0') {
        snprintf(line, sizeof(line), "+GTIFEVT: %s,cause=%d\r\n", reason, cause);
        emit(line);
        return;
    }
    /* reason 缺失：保守输出 cause 事件，绝不猜 */
    snprintf(line, sizeof(line), "+GTIFEVT: unknown,cause=%d\r\n", cause);
    emit(line);
}

/* 打开（或重开）URC 注入口。raw 无所谓 —— 只写不读。 */
static void open_tty(void)
{
    g_tty = open(TTY_OUT, O_WRONLY | O_NOCTTY | O_NONBLOCK);
    if (g_tty < 0) {
        syslog(LOG_WARNING, "open %s failed: %s", TTY_OUT, strerror(errno));
        return;
    }
    syslog(LOG_INFO, "URC sink ready: %s", TTY_OUT);
}

int main(void)
{
    openlog("atproxy", LOG_PID | LOG_NDELAY, LOG_DAEMON);

    signal(SIGTERM, on_signal);
    signal(SIGINT, on_signal);
    signal(SIGPIPE, SIG_IGN);

    open_tty(); /* 开不出来也不退出：可能比 usb.init 早，主循环里重试 */

    if (mipc_init("atproxy") != 0) {
        syslog(LOG_ERR, "mipc_init failed, exit");
        return 1;
    }
    if (mipc_msg_register_ind_api(0x10, MIPC_IND_EIF, on_eif_ind, NULL, NULL) != 0) {
        syslog(LOG_ERR, "register EIF_IND failed, exit");
        mipc_deinit();
        return 1;
    }
    syslog(LOG_INFO, "atproxy started: EIF_IND(0x4205) -> +GT* on %s", TTY_OUT);

    while (g_run) {
        sleep(2);
        if (g_tty < 0)
            open_tty(); /* USB 重枚举后自愈 */
    }

    syslog(LOG_INFO, "atproxy stopping");
    mipc_deinit();
    if (g_tty >= 0)
        close(g_tty);
    closelog();
    return 0;
}
