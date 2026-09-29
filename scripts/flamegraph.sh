#!/usr/bin/env bash
# CPU flamegraph of a running service (Linux, perf + inferno).
#
#   scripts/flamegraph.sh build                 build the services for profiling
#   scripts/flamegraph.sh <service> [seconds]   sample it while load runs
#
# The profiling profile keeps symbols and line tables; frame pointers make
# perf's stack walking cheap and exact (std has them since Rust 1.79), and
# --no-rosegment keeps lld's layout readable to perf. On Docker Desktop
# (WSL2) run inside a --privileged container and sample `cpu-clock`: the
# VM exposes no hardware counters. Needs perf (linux-perf / linux-tools) and
# inferno (cargo install inferno). SVGs land in target/flamegraphs/.
set -euo pipefail

if [[ "${1:-}" == "build" ]]; then
  RUSTFLAGS="-C force-frame-pointers=yes -C link-arg=-Wl,--no-rosegment" \
    cargo build --profile profiling -p gateway -p signaling -p relay
  exit
fi

service=${1:?usage: $0 build | <gateway|signaling|relay> [seconds]}
seconds=${2:-30}
pid=$(pgrep -x "$service" | head -n 1) || { echo "no running $service" >&2; exit 1; }
out=target/flamegraphs
mkdir -p "$out"
data=$(mktemp)
trap 'rm -f "$data"' EXIT

perf record -e cpu-clock -F 997 -g --call-graph fp -p "$pid" -o "$data" -- sleep "$seconds"
svg="$out/$service-$(date +%Y%m%d-%H%M%S).svg"
perf script -i "$data" | inferno-collapse-perf | inferno-flamegraph --title "$service ($seconds s)" > "$svg"
echo "$svg"
