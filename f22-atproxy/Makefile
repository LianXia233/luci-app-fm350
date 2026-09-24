# atproxy —— F22 AP 侧 EIF 语义代理
# 交叉编译：aarch64 musl（与 F22 root.squashfs 的 musl libc ABI 兼容）
# 用法：make TOOLCHAIN=/opt/aarch64-linux-musl-cross/bin/aarch64-linux-musl-
#
# 链接策略：动态链接固件自带的 libmipc_msg.so 与 musl libc.so —— 不向目标
# rootfs 引入任何新库（约束：只用原固件依赖）。
# 只链 libmipc_msg.so 是与官方 atcid / mtk_netagent 对齐的形态：mipc_init /
# mipc_msg_register_ind_api / mipc_msg_get_val_ptr / mipc_deinit 全部由
# libmipc_msg.so 导出，二者均不 NEEDED libmipc_api.so（它反而拖进 uClibc++
# 依赖）。交叉工具链的 sysroot 只用于编译期头文件；运行期解析全部由固件
# /usr/lib 提供。为让链接器接受「运行时在目标机上」的库，编译时以
# -L / -rpath-link 指向从固件解包树拷出的 .so 副本，并在 -rpath 留固件原生路径。

TOOLCHAIN ?= aarch64-linux-musl-
CC       := $(TOOLCHAIN)gcc
MIPC_DIR ?= ../libs-f22          # 存放 libmipc_msg.so 的副本（固件解包树 usr/lib）
CFLAGS   := -O2 -Wall -Wextra -std=c99 -D_GNU_SOURCE
LDFLAGS  := -L$(MIPC_DIR) -Wl,-rpath-link,$(MIPC_DIR),-rpath,/usr/lib
LDLIBS   := -lmipc_msg

TARGET := atproxy

all: $(TARGET)

$(TARGET): src/atproxy.c
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS) $(LDLIBS)

strip: $(TARGET)
	$(TOOLCHAIN)strip $@

clean:
	rm -f $(TARGET)

.PHONY: all strip clean
