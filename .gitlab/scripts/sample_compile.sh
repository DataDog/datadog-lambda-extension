#!/bin/bash

# Throwaway CI diagnostic for the bottlecap compile timeouts.
# Branch lpimentel/spike-compile-sampler. Do not merge.
#
# Runs the real compile script unchanged and samples the job container while it
# runs. The point is to capture container, node, and network state during one of
# the stalls, since the normal traces only show that output stopped.
#
# /proc is not namespaced for loadavg, meminfo, sockstat, or /proc/net/snmp, so
# those samples describe the runner node, not just this container.

set -u

SAMPLER_LOG=${SAMPLER_LOG:-sampler.log}
BUILD_LOG=${BUILD_LOG:-build.log}
# Overridable so the script can be exercised locally with a stub compile.
SAMPLE_INTERVAL=${SAMPLE_INTERVAL:-10}
# Output gap, in seconds, that triggers a detailed snapshot.
SILENCE_THRESHOLD=${SILENCE_THRESHOLD:-120}
# Minimum spacing between detailed snapshots, so a long stall does not flood the log.
DETAIL_INTERVAL=${DETAIL_INTERVAL:-180}
# Bound the compile so the script exits cleanly and uploads artifacts.
BUILD_LIMIT=${BUILD_LIMIT:-25m}
COMPILE_SCRIPT=${COMPILE_SCRIPT:-.gitlab/scripts/compile_bottlecap.sh}

: >"$SAMPLER_LOG"
: >"$BUILD_LOG"

ts() { date -u +%FT%TZ; }

have() { command -v "$1" >/dev/null 2>&1; }

# Never let a diagnostic command hang the sampler.
bounded() {
    local out rc
    out=$(timeout 15 "$@" 2>&1)
    rc=$?
    printf '%s' "$out" | head -40
    if [ "$rc" -eq 124 ]; then
        echo "(timed out after 15s: $*)"
    elif [ "$rc" -eq 127 ]; then
        echo "(unavailable: $*)"
    fi
}

proc_summary() {
    awk '/^(MemTotal|MemAvailable|SwapTotal|SwapFree):/ {printf "%s %s ", $1, $2}' /proc/meminfo
}

tcp_counters() {
    # Field 1 is the literal "Tcp:", so InSegs is 11, OutSegs is 12, RetransSegs is 13.
    awk '/^Tcp:/ {n++; if (n == 2) {print "InSegs=" $11 " OutSegs=" $12 " RetransSegs=" $13; exit}}' /proc/net/snmp
}

tcp_state_counts() {
    awk 'NR > 1 {c[$4]++} END {for (s in c) printf "state_%s=%d ", s, c[s]}' /proc/net/tcp
}

socket_summary() {
    tr '\n' ' ' </proc/net/sockstat 2>/dev/null || echo "(no sockstat)"
}

pressure_summary() {
    for f in /proc/pressure/cpu /proc/pressure/io /proc/pressure/memory /sys/fs/cgroup/cpu.pressure /sys/fs/cgroup/memory.pressure; do
        [ -r "$f" ] && printf '%s: %s | ' "$f" "$(tr '\n' ';' <"$f")"
    done
    echo
}

compact_sample() {
    {
        printf '%s loadavg=%s\n' "$(ts)" "$(cut -d' ' -f1-3 /proc/loadavg)"
        printf '  mem: %s\n' "$(proc_summary)"
        printf '  tcp: %s\n' "$(tcp_counters)"
        printf '  sockstat: %s\n' "$(socket_summary)"
        printf '  tcp_states: %s\n' "$(tcp_state_counts)"
        if have ss; then
            printf '  ss: %s\n' "$(bounded ss -s | tr '\n' ' ')"
        fi
        if have ps; then
            printf '  ps: %s\n' "$(ps -o pid,stat,rss,etime,comm 2>/dev/null | tail -n +2 | tr '\n' ';' | cut -c1-400)"
        fi
    } | tee -a "$SAMPLER_LOG"
}

detail_sample() {
    local reason="$1"
    {
        echo
        echo "===== DETAIL $(ts) ($reason) ====="
        echo "-- node --"
        echo "loadavg: $(cat /proc/loadavg)"
        printf 'mem: %s\n' "$(proc_summary)"
        echo "pressure: $(pressure_summary)"
        echo "-- build log age --"
        local mtime age
        mtime=$(stat -c %Y "$BUILD_LOG" 2>/dev/null || echo 0)
        age=$(( $(date +%s) - mtime ))
        echo "last write: $(date -u -d "@$mtime" +%FT%TZ 2>/dev/null || echo "epoch $mtime"), age ${age}s"
        echo "-- build log tail --"
        tail -c 1200 "$BUILD_LOG" | sed 's/^/  /'
        echo "-- tcp --"
        echo "counters: $(tcp_counters)"
        echo "states: $(tcp_state_counts)"
        echo "sockstat: $(socket_summary)"
        if have ss; then
            echo "ss summary: $(bounded ss -s | tr '\n' ' ')"
            echo "ss established: $(bounded ss -tnp state established | tail -n +2 | head -25 | tr '\n' ';' | cut -c1-800)"
            echo "ss retransmits: $(ss -tni 2>/dev/null | grep -o 'retrans:[0-9/]*' | sort | uniq -c | tr '\n' ';')"
        fi
        if have ps; then
            echo "-- processes --"
            { ps -o pid,ppid,stat,pcpu,rss,etime,args 2>/dev/null || ps 2>/dev/null || echo "(ps unavailable)"; } | head -20 | sed 's/^/  /'
        fi
        echo "-- buildx client view --"
        echo "buildx ls: $(bounded docker buildx ls | tr '\n' ';' | cut -c1-600)"
        echo "buildx inspect: $(bounded docker buildx inspect ci 2>&1 | grep -iE 'name|driver|status|endpoint|nodes|error' | tr '\n' ';' | cut -c1-600)"
        echo "-- disk --"
        df -h / /tmp 2>/dev/null | sed 's/^/  /'
        echo
    } | tee -a "$SAMPLER_LOG"
}

echo "sampler: interval ${SAMPLE_INTERVAL}s, silence threshold ${SILENCE_THRESHOLD}s, build limit ${BUILD_LIMIT}"
compact_sample

echo "phase: compile start $(ts)"
timeout -s TERM "$BUILD_LIMIT" "$COMPILE_SCRIPT" >"$BUILD_LOG" 2>&1 &
build_pid=$!

# Runs until the build is reaped and the parent stops it.
sampler_loop() {
    local last_detail=0 now mtime silence
    while :; do
        sleep "$SAMPLE_INTERVAL"
        compact_sample

        mtime=$(stat -c %Y "$BUILD_LOG" 2>/dev/null || echo 0)
        silence=$(( $(date +%s) - mtime ))
        if [ "$silence" -ge "$SILENCE_THRESHOLD" ]; then
            now=$(date +%s)
            if [ $((now - last_detail)) -ge "$DETAIL_INTERVAL" ]; then
                last_detail=$now
                detail_sample "no build output for ${silence}s"
            fi
        fi
    done
}
sampler_loop &
sampler_pid=$!

wait "$build_pid"
build_rc=$?
kill "$sampler_pid" 2>/dev/null || true
wait "$sampler_pid" 2>/dev/null || true

echo "phase: compile end $(ts), exit ${build_rc}"
compact_sample
detail_sample "compile finished with exit ${build_rc}"
echo "sampler: compile exit code ${build_rc} (124 means the ${BUILD_LIMIT} bound was hit)"

# Keep the pipeline meaningful: a timeout is the outcome we want to observe,
# so it is reported as success with the reason recorded in the log.
if [ "$build_rc" -eq 124 ]; then
    echo "sampler: build hit the ${BUILD_LIMIT} bound without completing"
    exit 0
fi

exit "$build_rc"
