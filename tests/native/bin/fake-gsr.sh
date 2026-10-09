#!/usr/bin/env bash
# Fake gpu-screen-recorder for native recorder tests. Honors an exit-code file,
# appends every control signal it receives to `fake-signals` (USR1 or the
# signal number), and idles like the replay-buffer child. The control/data
# directory is derived from the -sc hook path so parallel tests stay isolated.
set -u

# Install signal handling first so control signals can never kill a
# just-spawned fake (the recorder's stability wait is short in tests). Some
# bash builds reject the RTMIN name, so ignore SIGRTMIN through SIGRTMIN+6
# (glibc numbering) numerically.
signals="USR1 34 35 36 37 38 39 40"
for sig in $signals; do trap '' "$sig" 2>/dev/null; done
trap 'exit 0' INT TERM

if [ "${1:-}" = "--version" ]; then
  echo "fake gpu-screen-recorder 6.1.3"
  exit 0
fi

if [ "${1:-}" = "--list-audio-devices" ]; then
  cat << 'DEVICES'
default_output|Default output
default_input|Default input
alsa_output.pci.analog-stereo.monitor|Monitor of Built-in Analog Stereo
alsa_input.usb-mic|Fake USB Microphone
DEVICES
  exit 0
fi

data_dir=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "-sc" ]; then
    data_dir=$(dirname -- "$arg")
  fi
  prev="$arg"
done

if [ -n "$data_dir" ] && [ -f "$data_dir/fake-exit" ]; then
  exit "$(cat "$data_dir/fake-exit")"
fi

if [ -n "$data_dir" ]; then
  for sig in $signals; do
    trap "echo $sig >> \"\$data_dir/fake-signals\"" "$sig" 2>/dev/null
  done
fi

while :; do sleep 0.05; done
