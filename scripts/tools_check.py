# -*- coding: utf-8 -*-
# 取证：dbclient/dropbearkey 可用性 + 旧服务 31716 真身
import paramiko

def main():
    ssh = paramiko.SSHClient()
    ssh.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    ssh.connect("192.168.10.1", username="root", password="A1187900",
                allow_agent=False, look_for_keys=False, timeout=15)
    def run(name, cmd):
        _, so, se = ssh.exec_command(cmd, timeout=30)
        print("--- %s ---" % name)
        print(so.read().decode("utf-8", "replace").strip())
        e = se.read().decode("utf-8", "replace").strip()
        if e:
            print("[stderr]", e[:150])
    run("dbclient", "command -v dbclient || echo MISSING")
    run("dropbearkey", "command -v dropbearkey || echo MISSING")
    run("legacy 31716", "tr '\\000' ' ' < /proc/31716/cmdline 2>/dev/null; echo; "
                        "readlink /proc/31716/exe 2>/dev/null; "
                        "grep PPid /proc/31716/status 2>/dev/null")
    run("procd instances", "ubus call service list 2>/dev/null | grep -A3 'fm350-ssh' | head -8")
    run("init files", "ls /etc/init.d/ | grep -i 'fm350\\|ssh'")
    ssh.close()

if __name__ == "__main__":
    main()
