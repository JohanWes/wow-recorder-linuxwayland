#!/usr/bin/env bash
# Fake FFmpeg for native media-job tests. Records argv, honors a mode file, and
# writes progress/stderr the way the real binary does. The control directory is
# derived from the -progress path so parallel tests stay isolated.
#
# Modes (contents of <progress dir>/fake-ffmpeg-mode, default "ok"):
#   ok      write the output file, report one progress record, exit 0
#   fail    write stderr diagnostics and exit 1
#   silent  ignore SIGINT and never print anything (forces the kill escalation)
#   chatty  stream stderr and progress until SIGINT
set -u

if [ "${1:-}" = "-version" ] || [ "${1:-}" = "--version" ]; then
  echo "fake ffmpeg version 0"
  exit 0
fi

progress=""
output=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "-progress" ]; then
    progress="$arg"
  fi
  prev="$arg"
  output="$arg"
done

dir=$(dirname -- "${progress:-.}")
mode="ok"
[ -f "$dir/fake-ffmpeg-mode" ] && mode=$(cat "$dir/fake-ffmpeg-mode")
out_us=3000000
[ -f "$dir/fake-ffmpeg-out-us" ] && out_us=$(cat "$dir/fake-ffmpeg-out-us")

if [ -n "$progress" ]; then
  : > "$dir/fake-ffmpeg-argv.txt"
  for arg in "$@"; do
    printf '%s\n' "$arg" >> "$dir/fake-ffmpeg-argv.txt"
  done
fi

case "$mode" in
  ok)
    printf 'fake ffmpeg media' > "$output"
    printf 'frame=1\nout_time_us=%s\nprogress=end\n' "$out_us" >> "$progress"
    exit 0
    ;;
  fail)
    echo "fake ffmpeg: deliberate failure detail" >&2
    exit 1
    ;;
  silent)
    trap '' INT TERM
    while :; do sleep 0.05; done
    ;;
  chatty)
    trap 'exit 130' INT
    count=0
    while :; do
      count=$((count + 1))
      printf 'out_time_us=%s\nprogress=continue\n' "$((count * 100000))" >> "$progress"
      echo "fake ffmpeg noise line $count" >&2
      sleep 0.05
    done
    ;;
  *)
    echo "fake ffmpeg: unknown mode $mode" >&2
    exit 2
    ;;
esac
