#!/usr/bin/env python3
# 云服务器上执行：patch dropbear 2024.86 svr-authpasswd.c
# 空哈希账户 + svr_opts.allowblankpass(-B) 时允许登录（对齐 OpenWrt 出厂行为）。
# 精确文本替换，count==1 断言，替换后复核。

path = "src/svr-authpasswd.c"
with open(path, "r", encoding="utf-8", newline="") as f:
    src = f.read()

old = (
    "\t/* check for empty password */\n"
    "\tif (passwdcrypt[0] == '\\0') {\n"
    "\t\tdropbear_log(LOG_WARNING, \"User '%s' has blank password, rejected\",\n"
    "\t\t\t\tses.authstate.pw_name);\n"
    "\t\tsend_msg_userauth_failure(0, 1);\n"
    "\t\treturn;\n"
    "\t}\n"
)
new = (
    "\t/* check for empty password */\n"
    "\tif (passwdcrypt[0] == '\\0') {\n"
    "\t\tif (!svr_opts.allowblankpass) {\n"
    "\t\t\tdropbear_log(LOG_WARNING, \"User '%s' has blank password, rejected\",\n"
    "\t\t\t\t\tses.authstate.pw_name);\n"
    "\t\t\tsend_msg_userauth_failure(0, 1);\n"
    "\t\t\treturn;\n"
    "\t\t}\n"
    "\t\t/* F22: blank shadow hash + -B allows login. USB-only surface,\n"
    "\t\t * same trust level as the always-on adbd root shell. */\n"
    "\t\tdropbear_log(LOG_NOTICE, \"Password auth succeeded for '%s' from %s\",\n"
    "\t\t\t\tses.authstate.pw_name, svr_ses.addrstring);\n"
    "\t\tsend_msg_userauth_success();\n"
    "\t\treturn;\n"
    "\t}\n"
)

cnt = src.count(old)
assert cnt == 1, "PATCH-FAIL: target count=%d (expect 1)" % cnt
with open(path, "w", encoding="utf-8", newline="") as f:
    f.write(src.replace(old, new))

# 复核落盘
with open(path, "r", encoding="utf-8", newline="") as f:
    chk = f.read()
assert chk.count("allowblankpass") == 1 and chk.count("send_msg_userauth_success();") >= 1
assert old not in chk
print("PATCH-OK: blankpass branch installed")
