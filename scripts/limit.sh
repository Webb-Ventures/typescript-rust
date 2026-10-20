#!/usr/bin/env bash
# Run a command with a hard memory cap and timeout (macOS has no cgroups and ignores ulimit -v).
# Usage: scripts/limit.sh [--mem-mb N] [--time-s N] -- <command...>
# Defaults: 4096 MB total RSS for the whole process tree, 600 s. Kills the tree when either is exceeded,
# and whatever the command left running in its process group when it exits.
set -u
mem_mb=4096; time_s=600
while [ $# -gt 0 ]; do
  case "$1" in
    --mem-mb) mem_mb=$2; shift 2 ;;
    --time-s) time_s=$2; shift 2 ;;
    --) shift; break ;;
    *) break ;;
  esac
done
[ $# -gt 0 ] || { echo "usage: scripts/limit.sh [--mem-mb N] [--time-s N] -- cmd..." >&2; exit 2; }

set -m                      # run the command in its own process group
"$@" &
pid=$!
pgid=$(ps -o pgid= -p "$pid" | tr -d ' ')
own_pgid=$(ps -o pgid= -p $$ | tr -d ' ')
start=$(date +%s)
limit_kb=$((mem_mb * 1024))

# With job control bash puts each background job in a new group whose id is the job's pid,
# set from the parent before `$!` exists, so no tty is needed. A command that exits at once
# (`sh -c 'x &'`) can already be reaped by bash's SIGCHLD handler when `ps` runs (on Linux it
# usually is), and `ps` prints nothing: its group is still `$pid`, holding what it left
# running. If setpgid had failed, there is no group `$pid` and killing it does nothing.
if [ -z "$pgid" ] && [[ $- == *m* ]]; then pgid=$pid; fi

# Normally the command leads its own process group. If job control could not give it one
# (bash prints "setpgid ... Operation not permitted"), never measure or kill our caller's
# group: track the command's descendants by parent pid instead, plus every descendant seen
# so far that is still in our group (a child whose parent exited is reparented away from
# the tree, but must still be measured and killed).
seen=" "
if [ -n "$pgid" ] && [ "$pgid" != "$own_pgid" ]; then
  grouped=1
  tree_pids() { ps -axo pid=,pgid= | awk -v g="$pgid" '$2==g {print $1}'; }
else
  grouped=0
  echo "limit.sh: no separate process group; tracking descendants of $pid" >&2
  seen_pids() {
    ps -axo pid=,pgid= | awk -v seen="$seen" -v g="$own_pgid" -v self=$$ \
      '$2==g && $1!=self && index(seen, " " $1 " ") {print $1}'
  }
  tree_pids() {
    { ps -axo pid=,ppid= | awk -v root="$pid" '
        { kids[$2] = kids[$2] " " $1 }
        END { q[1] = root; n = 1; for (i = 1; i <= n; i++) { print q[i]; m = split(kids[q[i]], k, " "); for (j = 1; j <= m; j++) q[++n] = k[j] } }'
      seen_pids; } | sort -un
  }
fi
rss_kb() {
  local pids; pids=$(printf '%s,' $1); pids=${pids%,}
  [ -n "$pids" ] || { echo 0; return; }
  ps -o rss= -p "$pids" 2>/dev/null | awk '{s+=$1} END {print s+0}'
}
kill_tree() {
  if [ "$grouped" = 1 ]; then kill -KILL -- "-$pgid" 2>/dev/null
  else tree_pids | xargs kill -KILL 2>/dev/null; fi
}

# Stopped from outside: take the command's tree down too, or it outlives us unlimited.
trap 'kill_tree; exit 143' TERM INT HUP

while kill -0 "$pid" 2>/dev/null; do
  pids=$(tree_pids | tr '\n' ' ')
  [ "$grouped" = 1 ] || seen=" $pids"
  rss=$(rss_kb "$pids")
  if [ "$rss" -gt "$limit_kb" ]; then
    echo "limit.sh: killed: memory $((rss / 1024)) MB > ${mem_mb} MB: $*" >&2
    kill_tree; wait "$pid" 2>/dev/null; exit 137
  fi
  if [ $(( $(date +%s) - start )) -gt "$time_s" ]; then
    echo "limit.sh: killed: ran longer than ${time_s}s: $*" >&2
    kill_tree; wait "$pid" 2>/dev/null; exit 124
  fi
  sleep 0.05
done
wait "$pid"
status=$?
# The command is done. Whatever it left running (`cmd &` in a package's test script) would
# escape the limits and hold the caller's output pipes open: kill it. ($pid itself is reaped
# and may be reused, so the fallback kills only the descendants it saw.)
if [ "$grouped" = 1 ]; then kill -KILL -- "-$pgid" 2>/dev/null
else seen_pids | xargs kill -KILL 2>/dev/null; fi
exit "$status"
