#!/usr/bin/env python3
"""patch_usbinit_adb.py — F22 rootfs usb.init ADB 启用补丁（v2/embind3）

对解包后的 rootfs 执行行级精准插入，启用 AP 域 adbd_usb：

- boot_mode 0001 分支（4x ACM，原厂无 ADB）：补 f5=ffs.adb 链接 + 启动 adbd_usb
- default 分支（已链 ffs.adb 但原厂不启动 adbd_usb）：补启动 adbd_usb
- boot_mode 0000 分支原厂已启动 adbd_usb，不变

用法:
    python3 patch_usbinit_adb.py <rootfs>/etc/init.d/usb.init

补丁前自动写 usb.init.orig 备份；重复执行安全（检测已含补丁则跳过）。
插入后断言 diff 仅新增行，配合 sh -n 做语法校验。
"""
import shutil
import sys

ADB_START = [
    '            echo "start adbd_usb"\n',
    '            /sbin/adbd_usb 1000>&- &\n',
]
ADB_F5 = [
    '            ln -sf /config/usb_gadget/g1/functions/ffs.adb '
    '/config/usb_gadget/g1/configs/b.1/f5\n',
] + ADB_START


def find(lines, pred, desc):
    for i, l in enumerate(lines):
        if pred(l):
            return i
    raise SystemExit("PATCH-FAIL: pattern not found: " + desc)


def main():
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    path = sys.argv[1]

    with open(path, "r", encoding="utf-8", newline="") as f:
        text = f.read()

    if "start adbd_usb" in text and text.count("start adbd_usb") >= 3:
        print("PATCH-SKIP: adbd_usb already enabled in all branches")
        return

    lines = text.splitlines(keepends=True)

    # default 分支: ffs.adb->f4 且下一行是 ;;（0000 分支的 f4 后是 umsg 条件行, 不会误中）
    i_default = find(lines, lambda l: "boot_mode default acm" in l,
                     "default branch marker")
    i_f4_default = None
    for i in range(i_default, len(lines)):
        if "ffs.adb" in lines[i] and "/f4" in lines[i]:
            i_f4_default = i
            break
    if i_f4_default is None:
        raise SystemExit("PATCH-FAIL: default branch f4 not found")
    if lines[i_f4_default + 1].strip() != ";;":
        raise SystemExit("PATCH-FAIL: default f4 not followed by ;;")

    # 0001 分支: acm.gs3->f4（全文件唯一）
    i_f4_0001 = find(lines,
                     lambda l: "acm.gs3" in l and "/f4" in l,
                     "0001 branch acm.gs3 f4")
    if lines[i_f4_0001 + 1].strip() != ";;":
        raise SystemExit("PATCH-FAIL: 0001 f4 not followed by ;;")

    # 从后往前插, 避免行号漂移
    lines[i_f4_default + 1:i_f4_default + 1] = ADB_START
    lines[i_f4_0001 + 1:i_f4_0001 + 1] = ADB_F5

    shutil.copyfile(path, path + ".orig")
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.writelines(lines)

    print("PATCH-OK: +%d lines (default:+2, 0001:+3); backup at %s.orig"
          % (len(ADB_START) + len(ADB_F5), path))


if __name__ == "__main__":
    main()
