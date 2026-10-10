# -*- coding: utf-8 -*-
# socat 秒死取证：手动 spawn + 生命周期观察 + stderr 捕获
import paramiko

def main():
    ssh = paramiko.SSHClient()
    ssh.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    ssh.connect("192.168.10.1", username="root", password="A1187900",
                allow_agent=False, look_for_keys=False, timeout=15)
    try:
        def run(name, cmd, timeout=60):
            _, so, se = ssh.exec_command(cmd, timeout=timeout)
            print("--- %s ---" % name)
            print(so.read().decode("utf-8", "replace").strip())
            e = se.read().decode("utf-8", "replace").strip()
            if e:
                print("[stderr]", e[:200])

        run("legacy loop alive?", "pgrep -f 'fm350-ssh-fwd' && echo LEGACY-ALIVE || echo LEGACY-DEAD")
        run("legacy socat alive?", "ps w | grep -i 'socat' | grep -v grep || echo NO-SOCAT-PROC")
        run("current pidfile", "cat /var/run/fm350-sshfwd.pid 2>/dev/null; "
                               "kill -0 $(cat /var/run/fm350-sshfwd.pid 2>/dev/null) 2>/dev/null && echo ALIVE || echo DEAD")
        # 手动复现 daemon 的 spawn 方式（stderr 落盘）
        run("manual spawn", "nohup socat TCP4-LISTEN:2222,fork,reuseaddr TCP4:127.0.0.1:2223 "
                            ">/tmp/socat.err 2>&1 & echo $! > /tmp/socat-test.pid; "
                            "echo spawned-pid=$(cat /tmp/socat-test.pid)")
        run("after 3s", "sleep 3; P=$(cat /tmp/socat-test.pid); "
                        "kill -0 $P 2>/dev/null && echo ALIVE-$P || echo DEAD-$P; "
                        "netstat -tln | grep ':2222 ' || echo NO-LISTEN; "
                        "cat /tmp/socat.err")
        run("after 20s", "sleep 20; P=$(cat /tmp/socat-test.pid); "
                         "kill -0 $P 2>/dev/null && echo STILL-ALIVE-$P || echo DIED-$P; "
                         "netstat -tln | grep ':2222 ' || echo NO-LISTEN; "
                         "cat /tmp/socat.err")
        # 对照：直接前台跑 2 秒看输出
        run("foreground probe", "timeout 2 socat TCP4-LISTEN:2223,fork,reuseaddr TCP4:127.0.0.1:22 2>&1 | head -5; echo RC=$?")
        run("which socat ver", "socat -V 2>&1 | head -2")
    finally:
        ssh.close()

if __name__ == "__main__":
    main()
