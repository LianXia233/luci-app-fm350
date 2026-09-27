# -*- coding: utf-8 -*-
# 触发 Build Release workflow_dispatch（token 经 GH_TOKEN 环境变量，不落盘）
import json
import os
import sys
import time
import urllib.request

REPO = "LianXia233/luci-app-fm350"

def req(url, method="GET", body=None, accept="application/vnd.github+json"):
    r = urllib.request.Request(url, method=method, headers={
        "User-Agent": "ci-dispatch",
        "Authorization": "Bearer " + os.environ.get("GH_TOKEN", ""),
        "Accept": accept,
    }, data=json.dumps(body).encode() if body is not None else None)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(r, timeout=30) as resp:
        raw = resp.read().decode("utf-8")
        return json.loads(raw) if raw else None

def main():
    # 触发 dispatch：version 留空 -> 取 Makefile PKG_VERSION
    req("https://api.github.com/repos/%s/actions/workflows/release.yml/dispatches" % REPO,
        method="POST",
        body={"ref": "main", "inputs": {"version": ""}})
    print("DISPATCH-SENT")
    # 轮询确认 run 注册
    for i in range(20):
        time.sleep(15)
        d = req("https://api.github.com/repos/%s/actions/runs?event=workflow_dispatch&per_page=3" % REPO)
        runs = [r for r in d.get("workflow_runs", [])
                if r["name"] == "Build Release" and r["head_sha"].startswith("1bed748972")]
        if runs:
            r = runs[0]
            print("RUN-REGISTERED", r["id"], r["status"], r["html_url"])
            return 0
        print("WAITING (%d)" % (i + 1), flush=True)
    print("NOT-REGISTERED-IN-TIME")
    return 1

if __name__ == "__main__":
    sys.exit(main())
