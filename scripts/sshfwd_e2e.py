# -*- coding: utf-8 -*-
# SSH 转发端到端测试：开启 -> 冲突检测 -> 停旧服务 -> 接管 -> 链路前段验证
import sys
import time

import paramiko

HOST, USER, PASS = "192.168.10.1", "root", "A1187900"

def run(ssh, cmd, timeout=120):
    _, so, se = ssh.exec_command(cmd, timeout=timeout)
    return so.read().decode("utf-8", "replace").strip(), se.read().decode("utf-8", "replace").strip()

def main():
    ssh = paramiko.SSHClient()
    ssh.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    ssh.connect(HOST, username=USER, password=PASS,
                allow_agent=False, look_for_keys=False, timeout=15)
    try:
        def step(name, cmd, timeout=120):
            print("=== %s ===" % name, flush=True)
            out, err = run(ssh, cmd, timeout)
            print(out)
            if err:
                print("[stderr]", err[:300])
            return out

        # 0) 补写新 UCI 项并开启
        step("0 write uci opts", (
            "uci set fm350.main.sshfwd_enable='1'; "
            "uci set fm350.main.sshfwd_lan_port='2222'; "
            "uci set fm350.main.sshfwd_fwd_port='2223'; "
            "uci commit fm350; uci show fm350 | grep sshfwd"))

        # 1) 等待 daemon 下一轮巡检（15s 节流 + poll 周期），此刻旧独立服务仍占 2222
        print("waiting 45s for daemon tick...", flush=True)
        time.sleep(45)
        st = step("1 status: expect port_conflict=true", "/usr/sbin/fm350d sshfwd 2>&1")

        # 2) 停掉旧独立转发服务（本次集成的替代对象）
        step("2 stop legacy fm350-ssh-fwd", (
            "/etc/init.d/fm350-ssh-fwd stop 2>&1; "
            "/etc/init.d/fm350-ssh-fwd disable 2>&1; "
            "kill $(cat /var/run/fm350-ssh-fwd.pid 2>/dev/null) 2>/dev/null; "
            "pkill -f 'socat.*0.0.0.0:2222' 2>/dev/null; "
            "for p in $(pgrep -f 'fm350-ssh-fwd'); do kill $p 2>/dev/null; done; "
            "sleep 2; netstat -tln | grep ':2222 ' || echo PORT-2222-FREE"))

        # 3) 等新巡检器接管 2222
        print("waiting 25s for takeover...", flush=True)
        time.sleep(25)
        st = step("3 status after takeover", "/usr/sbin/fm350d sshfwd 2>&1")

        # 4) 链路前段验证：LAN 监听 + forward + adb（模组 adbd 离线为已知边界）
        step("4 listeners", "netstat -tln | grep -E ':2222|:2223'")
        step("5 forward list", "adb forward --list 2>&1")
        step("6 adb devices", "adb devices 2>&1")
        step("7 socat proc", "ps w | grep -v grep | grep 'TCP4-LISTEN:2222' | head -2")
        step("8 pid file", "cat /var/run/fm350-sshfwd.pid 2>/dev/null; "
                           "kill -0 $(cat /var/run/fm350-sshfwd.pid 2>/dev/null) 2>/dev/null && echo SOCAT-ALIVE")
        step("9 daemon log", "logread | grep 'SSH 转发' | tail -5")

        # 10) LAN 侧连接行为：ADB 离线时 socat 应立即关闭（而非挂死）
        step("10 lan connect closes fast",
             "t0=$(cut -d' ' -f1 /proc/uptime); "
             "(sleep 2) | timeout 5 socat -T5 - TCP:127.0.0.1:2222 2>/dev/null | head -c 40; "
             "echo; t1=$(cut -d' ' -f1 /proc/uptime); "
             "awk -v a=$t0 -v b=$t1 'BEGIN{printf \"elapsed=%.1fs\\n\", b-a}'")
        return 0
    finally:
        ssh.close()

if __name__ == "__main__":
    sys.exit(main())
