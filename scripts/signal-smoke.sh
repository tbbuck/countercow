#!/bin/bash
# Check that being killed leaves the terminal usable.
#
# `ratatui::restore` undoes raw mode and the alternate screen, but only if it is reached. A signal
# used to kill the process mid-frame, leaving the shell it was launched from unusable until
# `reset` — which is what `kill` and a dropped SSH connection both do. This drives that path.
#
# The counter session needs no such help: the runtime tears an EventPipe session down when the
# client's socket closes, measured at under three seconds after a SIGKILL.
#
#   scripts/signal-smoke.sh <pid> [TERM|HUP]
set -u

pid="${1:?usage: signal-smoke.sh <pid> [TERM|HUP]}"
signal="${2:-TERM}"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$root/target/debug/countercow"
capture="$(mktemp -t countercow-signal)"

if [ ! -x "$bin" ]; then
    echo "build first: cargo build" >&2
    exit 1
fi

# A stray from an earlier attempt is easy to signal by mistake, and the result then means nothing.
if [ -n "$(pgrep -x countercow)" ]; then
    echo "ABORT: countercow is already running" >&2
    exit 1
fi

# stdin from a sleep rather than the terminal: with no stdin the pty reaches EOF at once, and the
# bytes that arrive with it get read as a keypress, which detaches to the picker before the signal
# lands and quietly tests a different path from the one intended.
( sleep 300 | script -q "$capture" /bin/sh -c "stty rows 40 cols 120; exec '$bin' --pid $pid" >/dev/null 2>&1 & )
sleep 7

victim="$(pgrep -x countercow)"
if [ "$(echo "$victim" | wc -w | tr -d ' ')" != "1" ]; then
    echo "ABORT: expected exactly one countercow, found: $victim" >&2
    exit 1
fi

echo "sending SIG$signal to $victim"
kill -"$signal" "$victim"
sleep 3

status=0
if kill -0 "$victim" 2>/dev/null; then
    echo "exited: NO — the signal was caught but never acted on" >&2
    kill -9 "$victim" 2>/dev/null
    status=1
else
    echo "exited: yes"
fi

for marker in $'\033[?1049l:left the alternate screen' $'\033[?25h:restored the cursor'; do
    sequence="${marker%%:*}"
    label="${marker#*:}"
    if grep -qF -- "$sequence" "$capture"; then
        echo "$label: yes"
    else
        echo "$label: NO — the terminal is left unusable" >&2
        status=1
    fi
done

if ! grep -qF "Heap size" "$capture"; then
    echo "note: the dashboard was never drawn, so this proved little" >&2
    status=1
fi

pkill -f "sleep 300" 2>/dev/null
rm -f "$capture"
exit $status
