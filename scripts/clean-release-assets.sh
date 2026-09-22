#!/bin/sh
# 删除同一个 Release 内「其它版本」的包资产。
#
# 为什么需要这一步：`gh release upload --clobber` 只覆盖「同名」文件。本项目的
# 包名里带版本号（luci-app-fm350-1.0.1-r2.apk），当 PKG_RELEASE 递增时文件名
# 发生了变化，旧包不会被覆盖，而是与新版并存于同一个 Release。用户从 Release
# 页面下载时可能装到旧版本，而且除了文件名之外没有别的线索能看出来。
#
# 用法：clean-release-assets.sh <tag> <保留版本子串>
#   clean-release-assets.sh v1.0.1 1.0.1-r2
#
# 保留子串刻意不带前导连字符：.apk 用「-」连接版本
# （luci-app-fm350-1.0.1-r2.apk），.ipk 用「_」包裹版本
# （luci-app-fm350_1.0.1-r2_aarch64_cortex-a53.ipk），只有不带前导连字符的
# 子串形式能同时匹配两者。
#
# 清理范围只限本项目自己的包：文件名以「luci-app-fm350」开头、紧随其后是「-」或
# 「_」，且以 .apk 或 .ipk 结尾（实际包名只有
# luci-app-fm350-<版本>.apk 与 luci-app-fm350_<版本>_<架构>.ipk 两种形态）。
# 要求紧跟分隔符是为了排除 luci-app-fm3500-... 这类同前缀但不同名的包。
# SHA256SUMS、SDK 公钥等其它资产一律保留（SHA256SUMS 由后续上传以 --clobber
# 覆盖成新版）。
#
# 退出码：0 表示清理完成；非 0 表示读取资产列表或删除失败，交由流水线判红。

set -eu

tag="${1:-}"
keep="${2:-}"
if [ -z "${tag}" ] || [ -z "${keep}" ]; then
	echo "用法: clean-release-assets.sh <tag> <保留版本子串>" >&2
	exit 2
fi

# 赋值式命令替换的退出码即 gh 的退出码，配合 set -e 在读取失败时中止，
# 不把"读不到资产列表"误当成"没有过期资产"。
listing="$(gh release view "${tag}" --json assets --jq '.assets[].name')"

removed=0
kept=0
# 资产名按行分隔遍历。关掉通配展开，避免名字里的 * 或 ? 被 shell 当模式处理；
# 同时把 IFS 收窄成换行，使含空格的名字不会被拆开。
set -f
IFS='
'
for name in ${listing}; do
	[ -n "${name}" ] || continue
	case "${name}" in
		luci-app-fm350[-_]*.apk|luci-app-fm350[-_]*.ipk) ;;
		*) continue ;;
	esac
	case "${name}" in
		*"${keep}"*)
			kept=$((kept + 1))
			continue
			;;
	esac
	echo "删除过期资产: ${name}"
	gh release delete-asset "${tag}" "${name}" --yes
	removed=$((removed + 1))
done
unset IFS
set +f

echo "清理完成: 删除 ${removed} 个过期包, 保留 ${kept} 个匹配「${keep}」的包"
