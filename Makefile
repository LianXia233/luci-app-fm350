#
# luci-app-fm350 —— Fibocom FM350-GL 5G 模组管理（LuCI 界面 + Rust 后端）
#
# 说明：
#   1. 本 Makefile 为自包含实现，只依赖 rules.mk / package.mk，
#      不依赖 feeds/luci/luci.mk —— 因此在没有 LuCI feed 的 SDK 中同样可以构建。
#   2. 菜单通过 /usr/share/luci/menu.d/*.json 注册（LuCI 22+ 后的标准方式），
#      不使用 Lua 控制器，也不使用编译后的 .ut 模板。
#   3. 界面文案直接使用简体中文，不依赖 .lmo 翻译文件。
#
#   全新插件：独立使用 /etc/config/fm350 配置，后端二进制为 /usr/sbin/fm350d。
#
# 构建前置：
#   - cargo 与目标 target，例如 rustup target add aarch64-unknown-linux-musl
#   - 可用 CARGO=/path/to/cargo 覆盖
#

include $(TOPDIR)/rules.mk

PKG_NAME:=luci-app-fm350
PKG_VERSION:=1.0.6
PKG_RELEASE:=1
PKG_LICENSE:=GPL-3.0-only
PKG_MAINTAINER:=LianXia233
PKG_BUILD_PARALLEL:=1

# ---------------------------------------------------------------- Rust 交叉编译

RUST_DIR:=rust
RUST_BIN:=fm350d
RUST_PROFILE:=release
CARGO?=cargo

# 按目标架构选择 Rust target triple（musl 静态链接，避免目标机 libc 不匹配）
ifeq ($(ARCH),aarch64)
  RUST_TARGET:=aarch64-unknown-linux-musl
else ifeq ($(ARCH),x86_64)
  RUST_TARGET:=x86_64-unknown-linux-musl
else ifeq ($(ARCH),i386)
  RUST_TARGET:=i686-unknown-linux-musl
else ifeq ($(ARCH),arm)
  RUST_TARGET:=armv7-unknown-linux-musleabihf
else ifeq ($(ARCH),mipsel)
  RUST_TARGET:=mipsel-unknown-linux-musl
else ifeq ($(ARCH),riscv64)
  RUST_TARGET:=riscv64gc-unknown-linux-musl
else
  RUST_TARGET:=$(ARCH)-unknown-linux-musl
endif

# CARGO_TARGET_<TRIPLE>_LINKER 环境变量名（triple 中的 - 转 _，并大写）
RUST_LINKER_VAR:=CARGO_TARGET_$(shell echo $(RUST_TARGET) | tr 'a-z-' 'A-Z_')_LINKER

RUST_OUT:=$(CURDIR)/$(RUST_DIR)/target/$(RUST_TARGET)/$(RUST_PROFILE)/$(RUST_BIN)

# ---------------------------------------------------------------- 包定义

include $(INCLUDE_DIR)/package.mk

LUCI_HTDOCSDIR?=/www/luci-static/resources

define Package/$(PKG_NAME)
  SECTION:=luci
  CATEGORY:=LuCI
  SUBMENU:=3. Applications
  TITLE:=Fibocom FM350-GL 5G 模组管理（Rust 后端 fm350d）
  URL:=https://github.com/LianXia233/luci-app-fm350
  DEPENDS:=+luci-base +rpcd-mod-ucode +kmod-usb-net-rndis +kmod-usb-serial-option
endef

define Package/$(PKG_NAME)/description
LuCI 管理界面与 Rust 后端守护 fm350d，用于 Fibocom FM350-GL 模组的状态查询、
APN 与拨号配置、连接与断开、短信收发、锁频段与锁小区。
AT 端口由后端独占；IMEI 默认只读，写入需显式开启开关并二次确认。
endef

# ---------------------------------------------------------------- 构建

define Build/Prepare
	mkdir -p $(PKG_BUILD_DIR)
endef

define Build/Compile
	@echo "==> 编译 Rust 后端（target: $(RUST_TARGET)）"
	cd $(CURDIR)/$(RUST_DIR) && \
		$(RUST_LINKER_VAR)="$(TARGET_CROSS)gcc" \
		$(CARGO) build --$(RUST_PROFILE) --target $(RUST_TARGET)
	@test -f $(RUST_OUT) || (echo "Rust 产物缺失: $(RUST_OUT)" && exit 1)
	@echo "==> 后端二进制: $(RUST_OUT)"
	@if [ -n "$(TARGET_STRIP)" ] && [ -x "$(TARGET_STRIP)" ]; then \
		$(TARGET_STRIP) $(RUST_OUT) || true; \
	else \
		echo "==> 跳过 strip（TARGET_STRIP 不可用）"; \
	fi
endef

# ---------------------------------------------------------------- 安装

define Package/$(PKG_NAME)/install
	# 菜单注册
	$(INSTALL_DIR) $(1)/usr/share/luci/menu.d
	$(INSTALL_DATA) $(CURDIR)/root/usr/share/luci/menu.d/luci-app-fm350.json \
		$(1)/usr/share/luci/menu.d/luci-app-fm350.json

	# 前端资源（LuCI JS 视图与独立 CSS）
	$(INSTALL_DIR) $(1)$(LUCI_HTDOCSDIR)
	$(CP) $(CURDIR)/htdocs/luci-static/resources/* $(1)$(LUCI_HTDOCSDIR)/

	# 配置文件、init 脚本、rpcd ucode 插件与 ACL
	$(INSTALL_DIR) $(1)/etc/uci-defaults
	$(INSTALL_BIN) $(CURDIR)/root/etc/uci-defaults/99-fm350-network \
		$(1)/etc/uci-defaults/99-fm350-network
	$(INSTALL_DIR) $(1)/etc/config
	$(INSTALL_CONF) $(CURDIR)/root/etc/config/fm350 $(1)/etc/config/fm350
	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_BIN) $(CURDIR)/root/etc/init.d/$(RUST_BIN) $(1)/etc/init.d/$(RUST_BIN)
	$(INSTALL_DIR) $(1)/usr/share/rpcd/ucode
	$(INSTALL_DATA) $(CURDIR)/root/usr/share/rpcd/ucode/fm350.uc $(1)/usr/share/rpcd/ucode/fm350.uc
	$(INSTALL_DIR) $(1)/usr/share/rpcd/acl.d
	$(INSTALL_DATA) $(CURDIR)/root/usr/share/rpcd/acl.d/luci-app-fm350.json \
		$(1)/usr/share/rpcd/acl.d/luci-app-fm350.json

	# Rust 后端二进制
	$(INSTALL_DIR) $(1)/usr/sbin
	$(INSTALL_BIN) $(RUST_OUT) $(1)/usr/sbin/$(RUST_BIN)
endef

define Package/$(PKG_NAME)/postinst
#!/bin/sh
[ -n "$$$${IPKG_INSTROOT}" ] || {
	# 菜单/模块缓存必须清理，否则新菜单与 rpcd 插件不会立即生效
	rm -f /tmp/luci-indexcache*
	rm -rf /tmp/luci-modulecache/
	/etc/init.d/rpcd reload 2>/dev/null
	exit 0
}
endef

define Package/$(PKG_NAME)/prerm
#!/bin/sh
[ -n "$$$${IPKG_INSTROOT}" ] || {
	/etc/init.d/fm350d stop 2>/dev/null
	/etc/init.d/fm350d disable 2>/dev/null
	rm -f /tmp/luci-indexcache*
	exit 0
}
endef

$(eval $(call BuildPackage,$(PKG_NAME)))
