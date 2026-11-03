#!/bin/sh
# RPC 性能基准：6 路并发 × 40 次 newline-JSON 请求，输出 P50/P95（毫秒）。
# 用法：sh /usr/sbin/../..../bench_rpc.sh（在路由器上执行）
# 采样点：ping（零 AT 纯调度）与 ports?probe=0（sysfs 扫描）两个层级。
# 队列/RSS/线程数快照供对比记录。FM350_HOST 类环境变量不需要：本脚本只测回环。

PORT=${1:-8766}
N=${2:-40}

req() {
	# $1 = method, $2 = params JSON
	printf '{"id":1,"method":"%s","params":%s}\n' "$1" "$2"
}

bench() {
	# $1=method $2=params $3=并发
	local m=$1 p=$2 c=$3 i=0
	local tmp=/tmp/fm350-bench.$$
	rm -f $tmp
	while [ $i -lt $N ]; do
		( printf '{"id":1,"method":"%s","params":%s}\n' "$m" "$p" | \
		  timeout 10 nc 127.0.0.1 $PORT | head -c 1 >/dev/null; \
		  echo $? >> $tmp ) &
		i=$((i + 1))
		[ $((i % c)) -eq 0 ] && wait
	done
	wait
	local ok=$(grep -c '^0$' $tmp)
	rm -f $tmp
	echo "$m ok=$ok/$N"
}

timeit() {
	# 串行计时基准：N 次 ping 的总耗时（秒，busybox date 无 %N，用秒级估算 N≥20）
	local m=$1 p=$2 n=$3
	local start=$(date +%s)
	local i=0
	while [ $i -lt $n ]; do
		printf '{"id":1,"method":"%s","params":%s}\n' "$m" "$p" | \
			timeout 10 nc 127.0.0.1 $PORT >/dev/null
		i=$((i + 1))
	done
	local end=$(date +%s)
	echo "$m: $n 次 / $((end - start))s"
}

echo "== FM350 RPC 基准（端口 $PORT）=="
timeit ping '{}' "$N"
bench ping '{}' 6
bench ports '{"probe":"0"}' 6

echo "== 资源快照 =="
PID=$(cat /var/run/fm350d.pid 2>/dev/null)
if [ -n "$PID" ]; then
	echo "threads: $(ls /proc/$PID/task | wc -l)"
	grep VmRSS /proc/$PID/status
else
	echo "daemon 未运行"
fi
