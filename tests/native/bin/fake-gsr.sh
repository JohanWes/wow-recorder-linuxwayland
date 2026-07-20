#!/usr/bin/env bash
# Fake gpu-screen-recorder for native recorder tests. Records argv, honors an
# exit-code file, and idles like the replay-buffer child. The control/data
# directory is derived from the -sc hook path so parallel tests stay isolated.
set -u

# Install signal handling first so control signals can never kill a
# just-spawned fake (the recorder's stability wait is short in tests). Some
# bash builds reject the RTMIN name, so ignore the whole glibc RT range
# numerically.
trap '' USR1
for sig in 34 35 36 37 38; do trap '' "$sig" 2>/dev/null; done
trap 'exit 0' INT TERM

if [ "${1:-}" = "--version" ]; then
  echo "fake gpu-screen-recorder 5.13.9"
  exit 0
fi

if [ "${1:-}" = "--list-audio-devices" ]; then
  cat << 'DEVICES'
Output devices:
default_output|Default output
device:alsa_output.pci.analog-stereo|Built-in Analog Stereo
Input devices:
default_input|Default input
device:alsa_input.usb-mic|Fake USB Microphone
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

if [ -n "$data_dir" ]; then
  : > "$data_dir/fake-argv.txt"
  for arg in "$@"; do
    printf '%s\n' "$arg" >> "$data_dir/fake-argv.txt"
  done
  if [ -f "$data_dir/fake-exit" ]; then
    exit "$(cat "$data_dir/fake-exit")"
  fi
fi

while :; do sleep 0.05; done
