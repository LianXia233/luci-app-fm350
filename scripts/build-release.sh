#!/usr/bin/env bash
#
# luci-app-fm350 发布构建脚本
#
# 双格式构建：
#   apk —— ImmortalWrt SNAPSHOT（apk-tools 3，新主线）
#   ipk —— ImmortalWrt 24.10.3 稳定版（opkg，兼容老版本固件）
# 同一份源码分别进两个 SDK 编译，产物统一收集到 dist-release/。
#
# 产物：
#   dist-release/luci-app-fm350-<版本>-r<发布>.apk          （SNAPSHOT / apk）
#   dist-release/luci-app-fm350_<版本>-r<发布>_<架构>.ipk    （24.10 / opkg）
#   dist-release/SHA256SUMS
#   dist-release/openwrt-sdk-build.pem（SDK 自带公钥，便于校验包签名）
#
# 本脚本按 POSIX/Linux 取向编写，只在 Linux 上运行（CI 使用 ubuntu-latest）。
#
# 设计要点：
#   1. SDK 包名与 sha256 都从镜像站的 sha256sums 里读，不写死文件名——镜像站
#      更新快照后文件名会变，写死会让发布流程在某个周二早上突然红掉。
#   2. 断言只在 .pkgdir 暂存目录上做，不去解最终的包。OpenWrt 25.x 起
#      apk-tools 3 的容器格式是 ADB.pckg（魔数 "ADBd"），既不是 tar 也不是
#      gzip 流，每段独立压缩、偏移由索引表记录；按格式手工解析会跟着
#      apk-tools 版本漂移。.pkgdir 是 make 自己的中间产物，与打包器格式无关。
#   3. 断言"进包的文本文件与 git 源码逐字节一致"。这一条同时覆盖三种事故：
#      构建期间有人加了压缩步骤、行尾被 CRLF 污染、某个文件根本没被打进包。
#
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work_dir="${FM350_WORK_DIR:-${RUNNER_TEMP:-/tmp}/fm350-release-sdk}"
output_dir="${repo_dir}/dist-release"

rust_target="${FM350_RUST_TARGET:-aarch64-unknown-linux-musl}"
pkg_name="luci-app-fm350"

# 双格式 SDK 源（可用环境变量覆盖，便于本地/镜像切换）
sdk_snapshot="${FM350_SDK_SNAPSHOT:-https://downloads.immortalwrt.org/snapshots/targets/mediatek/filogic}"
sdk_2410="${FM350_SDK_2410:-https://downloads.immortalwrt.org/releases/24.10.3/targets/mediatek/filogic}"

step() { printf '\n==> %s\n' "$*"; }
fail() { printf '::error::%s\n' "$*" >&2; exit 1; }
note() { printf '    %s\n' "$*"; }

pkg_version="$(sed -n 's/^PKG_VERSION:=//p' "${repo_dir}/Makefile")"
pkg_release="$(sed -n 's/^PKG_RELEASE:=//p' "${repo_dir}/Makefile")"
[ -n "${pkg_version}" ] || fail "无法从 Makefile 读取 PKG_VERSION"
[ -n "${pkg_release}" ] || fail "无法从 Makefile 读取 PKG_RELEASE"

step "构建 ${pkg_name} ${pkg_version}-r${pkg_release}（apk + ipk 双格式）"

# ---------------------------------------------------------------- Rust 目标

step "确认 Rust 交叉编译目标 ${rust_target}"
# rustup 未必在 PATH 上：CI 的 setup-rust-toolchain 会放进去，但裸机/容器里
# 常见情况是 rustup 装在 ~/.cargo/bin 而 PATH 只显式带了 cargo。这里按
# PATH → $CARGO_HOME → ~/.cargo/bin → 常见位置依次查找，并把找到的目录前置到
# PATH，避免 rustup 与 cargo 来自两套不同的工具链。
rustup_bin="$(command -v rustup 2>/dev/null || true)"
if [ -z "${rustup_bin}" ]; then
	cargo_home_bin=""
	[ -n "${CARGO_HOME:-}" ] && cargo_home_bin="${CARGO_HOME}/bin/rustup"
	for cand in ${cargo_home_bin} "${HOME}/.cargo/bin/rustup" /usr/local/cargo/bin/rustup \
		/usr/local/rustup/bin/rustup /usr/bin/rustup; do
		if [ -x "${cand}" ]; then
			rustup_bin="${cand}"
			break
		fi
	done
fi
[ -n "${rustup_bin}" ] || fail "未找到 rustup；Makefile 用 cargo 交叉编译，需要 rustup 提供 ${rust_target} 标准库"
PATH="$(dirname "${rustup_bin}"):${PATH}"
export PATH
note "rustup: ${rustup_bin}"
if ! rustup target list --installed | grep -qx "${rust_target}"; then
	rustup target add "${rust_target}"
fi
rustup target list --installed | grep -qx "${rust_target}" || fail "Rust target ${rust_target} 不可用"
note "$(rustc --version) / $(cargo --version) / $(command -v cargo)"

# ---------------------------------------------------------------- 单格式构建

# 每个格式独立下载/解压 SDK 并在其中编译；产物与断言按格式分别执行。
build_one() {
	local name="$1" sdk_base="$2" fmt="$3"
	local sdk_work="${work_dir}/${name}" archive="" sdk_dir="" found=""
	local pkg_roots="" pkg_root_count=0 checked=0 pairs="" src_rel packed_rel src packed

	step "[${name}] 构建 ${fmt}（SDK 源：${sdk_base}）"
	mkdir -p "${sdk_work}"
	cd "${sdk_work}"

	# ---- 取 SDK
	archive=""
	if curl -fsSLO "${sdk_base}/sha256sums"; then
		archive="$(awk '/immortalwrt-sdk-.*Linux-x86_64\.tar\.zst$/ { print $2; exit }' sha256sums | sed 's/^\*//')"
	fi
	[ -n "${archive}" ] || fail "无法从 ${sdk_base}/sha256sums 解析出 SDK 包名"
	note "SDK 包名: ${archive}"

	sdk_dir=""
	if [ -d sdk ]; then
		sdk_dir="$(find sdk -maxdepth 1 -mindepth 1 -type d -name 'immortalwrt-sdk-*' | head -n 1 || true)"
	fi

	if [ -z "${sdk_dir}" ]; then
		step "[${name}] 下载 SDK（约 450 MB）"
		[ -f "${archive}" ] || curl -fL --retry 5 --retry-delay 3 -o "${archive}" "${sdk_base}/${archive}"
		step "[${name}] 校验 SDK sha256"
		grep "[ *]${archive}\$" sha256sums | sha256sum -c - || fail "SDK sha256 校验失败"
		step "[${name}] 解压 SDK（约 3 GB，耗时最长的一步）"
		rm -rf sdk
		mkdir sdk
		tar --zstd -xf "${archive}" -C sdk
		sdk_dir="$(find sdk -maxdepth 1 -mindepth 1 -type d -name 'immortalwrt-sdk-*' | head -n 1)"
		rm -f "${archive}"
	fi

	[ -n "${sdk_dir}" ] || fail "解压后未找到 SDK 目录"
	sdk_dir="$(cd "${sdk_dir}" && pwd)"
	[ -d "${sdk_dir}/scripts" ] || fail "SDK 目录结构异常：${sdk_dir}"
	note "SDK 目录: ${sdk_dir}"

	# ---- 源码进 SDK
	step "[${name}] 把源码放进 SDK 的 package/${pkg_name}"
	rm -rf "${sdk_dir:?}/package/${pkg_name}"
	mkdir -p "${sdk_dir}/package/${pkg_name}"
	cp -a "${repo_dir}/." "${sdk_dir}/package/${pkg_name}/"
	rm -rf "${sdk_dir}/package/${pkg_name}/.git" \
		"${sdk_dir}/package/${pkg_name}/.github" \
		"${sdk_dir}/package/${pkg_name}/rust/target" \
		"${sdk_dir}/package/${pkg_name}/dist-release"
	# SDK 侧的 Makefile 必须是 LF：CRLF 会让 `include` 路径带上 \r，报出几乎无法定位的错误
	grep -qU $'\r' "${sdk_dir}/package/${pkg_name}/Makefile" && fail "SDK 侧 Makefile 含 CR（行尾被转换）"
	note "已排除 .git / .github / rust/target / dist-release"

	# ---- feeds 与配置
	cd "${sdk_dir}"

	step "[${name}] 更新 feeds（首次克隆，约数分钟）"
	./scripts/feeds update -a
	./scripts/feeds install luci-base

	step "[${name}] 写入最小 .config 并 defconfig"
	cat > .config <<EOF
CONFIG_TARGET_mediatek=y
CONFIG_TARGET_mediatek_filogic=y
CONFIG_PACKAGE_${pkg_name}=m
# CONFIG_ALL is not set
# CONFIG_ALL_KMODS is not set
# CONFIG_ALL_NONSHARED is not set
EOF
	make defconfig

	grep -q "^CONFIG_PACKAGE_${pkg_name}=m\$" .config \
		|| fail "defconfig 之后 ${pkg_name} 不是 =m，检查包是否被 SDK 识别"
	if grep -qE '^CONFIG_ALL=y' .config; then
		fail "defconfig 把 CONFIG_ALL 打开了；SDK 的 Config.in 默认值结构已变，需要重新处理"
	fi
	note "CONFIG_PACKAGE_${pkg_name}=m 已确认"

	# ---- 编译
	step "[${name}] 编译（日志：${work_dir}/build-${name}.log）"
	if ! make "package/${pkg_name}/compile" -j"$(nproc)" V=s >"${work_dir}/build-${name}.log" 2>&1; then
		printf -- '---- 构建失败，日志中的错误行 ----\n' >&2
		grep -inE 'error(\[|:)|No rule to make|cannot find|undefined reference|failed to' "${work_dir}/build-${name}.log" \
			| tail -n 30 >&2 || true
		printf -- '---- 日志尾部 40 行 ----\n' >&2
		tail -n 40 "${work_dir}/build-${name}.log" >&2
		fail "make package/${pkg_name}/compile 失败，完整日志：${work_dir}/build-${name}.log"
	fi

	# ---- 收集产物
	step "[${name}] 收集 ${fmt} 产物"
	find bin -type f \( -name "${pkg_name}-*.apk" -o -name "${pkg_name}_*.ipk" \) -exec cp -f {} "${output_dir}/" \;

	# ---- 断言 1/3：产物存在且版本号与 Makefile 一致
	found="$(find bin -type f -name "${pkg_name}*.${fmt}" | head -n 1 || true)"
	[ -n "${found}" ] || {
		printf -- '---- bin 下的 fm350 产物 ----\n' >&2
		find bin -type f -name "${pkg_name}*" >&2 || true
		fail "[${name}] 未找到期望产物（${fmt}，版本号与 Makefile 不一致？）"
	}
	case "${found}" in
		*"${pkg_version}-r${pkg_release}"*) : ;;
		*) fail "[${name}] 产物版本号异常：$(basename "${found}")（期望 ${pkg_version}-r${pkg_release}）" ;;
	esac
	artifact_count="$(find "${output_dir}" -maxdepth 1 -type f \( -name '*.apk' -o -name '*.ipk' \) | wc -l)"
	[ "${artifact_count}" -ge 1 ] || fail "dist-release 中没有包产物"
	note "$(basename "${found}")（当前共 ${artifact_count} 个包产物）"

	# ---- 断言 2/3：进包的 fm350d 是 aarch64 可执行文件
	pkg_roots="$(find build_dir -type d -path "*${pkg_name}-*/.pkgdir/${pkg_name}" 2>/dev/null || true)"
	[ -n "${pkg_roots}" ] || fail "[${name}] 未找到 .pkgdir 暂存目录，无法校验进包内容"
	pkg_root_count=0
	for root in ${pkg_roots}; do
		pkg_root_count=$((pkg_root_count + 1))
		[ -f "${root}/usr/sbin/fm350d" ] || fail "[${name}] 包内缺少 /usr/sbin/fm350d（${root}）"
		[ -s "${root}/usr/sbin/fm350d" ] || fail "[${name}] 包内 fm350d 大小为 0（${root}）"
		file "${root}/usr/sbin/fm350d" | grep -q 'aarch64' \
			|| fail "[${name}] 包内 fm350d 不是 aarch64 可执行文件：$(file -b "${root}/usr/sbin/fm350d")"
		note "$(file -b "${root}/usr/sbin/fm350d")"
	done

	# ---- 断言 3/3：进包的文本文件与 git 源码逐字节一致
	pairs=""
	for rel in $(cd "${repo_dir}/htdocs" && find . -type f | sed 's|^\./||' | LC_ALL=C sort); do
		pairs="${pairs}htdocs/luci-static/resources/${rel}|www/luci-static/resources/${rel}
"
	done
	pairs="${pairs}root/usr/share/luci/menu.d/${pkg_name}.json|usr/share/luci/menu.d/${pkg_name}.json
root/usr/share/rpcd/acl.d/${pkg_name}.json|usr/share/rpcd/acl.d/${pkg_name}.json
root/usr/share/rpcd/ucode/fm350.uc|usr/share/rpcd/ucode/fm350.uc
root/etc/config/fm350|etc/config/fm350
root/etc/init.d/fm350d|etc/init.d/fm350d"

	checked=0
	while IFS='|' read -r src_rel packed_rel; do
		[ -n "${src_rel}" ] || continue
		src="${repo_dir}/${src_rel}"
		[ -f "${src}" ] || fail "源码缺失：${src_rel}"
		for root in ${pkg_roots}; do
			packed="${root}/${packed_rel}"
			[ -f "${packed}" ] || fail "[${name}] 未打进包：${packed_rel}（${root}）"
			cmp -s "${src}" "${packed}" || fail "[${name}] ${packed_rel} 与源码不一致（包内 $(wc -c <"${packed}") 字节 / 源码 $(wc -c <"${src}") 字节）"
			checked=$((checked + 1))
		done
	done <<EOF
${pairs}
EOF
	[ "${checked}" -gt 0 ] || fail "[${name}] 没有任何文件被比对，断言本身失效"
	note "已比对 ${checked} 处（${pkg_root_count} 个暂存目录）"

	# ---- 公钥（双 SDK 都收集，后者覆盖前者）
	if [ -f "${sdk_dir}/public-key.pem" ]; then
		cp -f "${sdk_dir}/public-key.pem" "${output_dir}/openwrt-sdk-build.pem"
	else
		note "[${name}] SDK 未提供 public-key.pem，本次不附带公钥"
	fi
}

# ---------------------------------------------------------------- 主流程

mkdir -p "${output_dir}"
find "${output_dir}" -mindepth 1 -maxdepth 1 -delete

build_one "snapshot" "${sdk_snapshot}" apk
build_one "2410" "${sdk_2410}" ipk

# ---------------------------------------------------------------- 汇总校验

step "断言 4/4：SHA256SUMS"
(cd "${output_dir}" && find . -maxdepth 1 -type f \( -name '*.apk' -o -name '*.ipk' -o -name 'openwrt-sdk-build.pem' \) -print0 \
	| LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS)

step "完成"
ls -la "${output_dir}"
cat "${output_dir}/SHA256SUMS"
echo "FM350-RELEASE-OK"
