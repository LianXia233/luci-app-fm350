# -*- coding: utf-8 -*-
# 复核：监听真实性 + ubus 经 rpcd 的 sshfwd 方法
import json
import paramiko

def main():
    ssh = paramiko.SSHClient()
    ssh.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    ssh.connect("192.168.10.1", username="root", password="A1187900",
                allow_agent=False, look_for_keys=False, timeout=15)
    try:
        def run(name, cmd):
            _, so, se = ssh.exec_command(cmd, timeout=60)
            print("--- %s ---" % name)
            print(so.read().decode("utf-8", "replace").strip())
            e = se.read().decode("utf-8", "replace").strip()
            if e:
                print("[stderr]", e[:200])

        pid = open("/dev/null").read() if False else None
        run("netstat -tlnp grep 2222", "netstat -tlnp 2>/dev/null | grep 2222; echo RC-DONE")
        run("netstat head", "netstat -tln | head -6")
        run("socat cmdline", "tr '\\000' ' ' < /proc/$(cat /var/run/fm350-sshfwd.pid)/cmdline; echo")
        run("tcp dump", "netstat -tn | grep -c 2222 || true")
        # ubus 经 rpcd 调 sshfwd（前端真实路径）
        run("ubus sshfwd", "ubus call luci.fm350 sshfwd 2>&1 | head -20")
    finally:
        ssh.close()

if __name__ == "__main__":
    main()
