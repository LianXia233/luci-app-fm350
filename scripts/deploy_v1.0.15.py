# -*- coding: utf-8 -*-
# 部署 luci-app-fm350 1.0.15-r1 到实机 192.168.10.1 并测试
# 用法: py -3 deploy_v1.0.15.py <stage>
#   stage: download | deploy | verify | sshfwd-test
import hashlib
import json
import os
import sys
import time
import urllib.request

import paramiko

HOST, USER, PASS = "192.168.10.1", "root", "A1187900"
MIRROR = "https://gh.acg2.mom/"
URL = "https://github.com/LianXia233/luci-app-fm350/releases/download/v1.0.15/luci-app-fm350-1.0.15-r1.apk"
SHA256 = "78bf88885e59c893a069ab56d6eb0a3e7ec89332c98e0385ea775b4e68a59cf8"
LOCAL = r"C:/Users/LX233/Documents/work/luci-app-fm350/dist-release/luci-app-fm350-1.0.15-r1.apk"
REMOTE_DIR = "/tmp/apk-install"

def download():
    os.makedirs(os.path.dirname(LOCAL), exist_ok=True)
    for url in (MIRROR + URL, URL):
        try:
            print("GET", url)
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            with opener.open(url, timeout=120) as r, open(LOCAL, "wb") as f:
                f.write(r.read())
            h = hashlib.sha256(open(LOCAL, "rb").read()).hexdigest()
            print("sha256", h)
            if h == SHA256:
                print("DOWNLOAD-OK", os.path.getsize(LOCAL), "bytes")
                return 0
            print("SHA-MISMATCH")
        except Exception as e:
            print("FAIL", e)
    return 1

def connect():
    ssh = paramiko.SSHClient()
    ssh.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    ssh.connect(HOST, username=USER, password=PASS,
                allow_agent=False, look_for_keys=False, timeout=15)
    return ssh

def run(ssh, cmd, timeout=120):
    _, so, se = ssh.exec_command(cmd, timeout=timeout)
    out = so.read().decode("utf-8", "replace").strip()
    err = se.read().decode("utf-8", "replace").strip()
    return out, err

def deploy():
    ssh = connect()
    try:
        sftp = ssh.open_sftp()
        try:
            sftp.mkdir(REMOTE_DIR)
        except IOError:
            pass
        sftp.put(LOCAL, REMOTE_DIR + "/pkg.apk")
        out, _ = run(ssh, "sha256sum %s/pkg.apk" % REMOTE_DIR)
        print("REMOTE:", out)
        if SHA256 not in out:
            print("REMOTE-SHA-MISMATCH")
            return 1
        out, err = run(ssh, "cd %s && apk add --simulate --allow-untrusted ./pkg.apk 2>&1" % REMOTE_DIR)
        print("--- simulate ---")
        print(out)
        if "trying to overwrite" in out:
            print("CONFLICT-DETECTED, abort")
            return 1
        out, err = run(ssh, "cd %s && apk add --allow-untrusted --clean-protected ./pkg.apk 2>&1" % REMOTE_DIR, timeout=180)
        print("--- install ---")
        print(out)
        if err:
            print("[stderr]", err)
        out, _ = run(ssh, "apk list -I 2>/dev/null | grep -i fm350")
        print("--- installed ---")
        print(out)
        # 收尾：清缓存 + 重启
        run(ssh, "rm -rf /tmp/luci-* /tmp/luci-modulecache; "
                 "/etc/init.d/rpcd restart; /etc/init.d/uhttpd restart; sleep 2")
        print("CLEANUP-DONE")
        return 0
    finally:
        ssh.close()

def verify():
    ssh = connect()
    try:
        checks = {
            "pkg": "apk list -I 2>/dev/null | grep -i fm350",
            "bin version": "/usr/sbin/fm350d --help 2>&1 | head -3; /usr/sbin/fm350d sshfwd 2>&1",
            "uci new opts": "uci show fm350 | grep -i sshfwd",
            "uci kept": "uci get fm350.main.v6_mode; uci get fm350.main.enabled",
            "menu/acl": "ls /usr/share/luci/menu.d/ | grep -i fm350; ls /usr/share/rpcd/acl.d/ | grep -i fm350",
            "ubus obj": "ubus list | grep luci.fm350",
            "service": "/etc/init.d/fm350d enabled && echo enabled; /etc/init.d/fm350d status 2>/dev/null || service fm350d status 2>/dev/null || true",
            "daemon log": "logread | grep -i fm350d | tail -3",
            "frontend file": "grep -c 'fm350-sshfwd\\|SSH 转发' /www/luci-static/resources/view/fm350/at.js 2>/dev/null || echo 0",
            "pkg-vs-repo files": "sha256sum /usr/share/rpcd/ucode/fm350.uc /etc/config/fm350 2>/dev/null",
        }
        for name, cmd in checks.items():
            out, err = run(ssh, cmd, timeout=60)
            print("=== %s ===" % name)
            print(out)
            if err:
                print("[stderr]", err[:200])
        return 0
    finally:
        ssh.close()

def sshfwd_test():
    ssh = connect()
    try:
        steps = [
            ("1 enable via uci",
             "uci set fm350.main.sshfwd_enable='1'; uci commit fm350; echo COMMITTED"),
            ("2 wait for daemon tick", ""),
            ("3 status after tick",
             "sleep 25; /usr/sbin/fm350d sshfwd 2>&1"),
            ("4 old fwd service state",
             "/etc/init.d/fm350-ssh-fwd status 2>&1; pgrep -f 'fm350-ssh-fwd' | head -3; netstat -tln | grep ':2222 '"),
            ("5 adb state", "adb devices 2>&1"),
        ]
        for name, cmd in steps:
            if not cmd:
                continue
            out, err = run(ssh, cmd, timeout=120)
            print("=== %s ===" % name)
            print(out)
            if err:
                print("[stderr]", err[:200])
        return 0
    finally:
        ssh.close()

if __name__ == "__main__":
    stage = sys.argv[1] if len(sys.argv) > 1 else "download"
    rc = {"download": download, "deploy": deploy,
          "verify": verify, "sshfwd-test": sshfwd_test}[stage]()
    sys.exit(rc)
