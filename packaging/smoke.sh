#!/bin/sh
# Smoke test for an installed Spotty: a click on the app icon starts it in
# the background, --settings opens Settings, --quit stops it.
# Usage: smoke.sh COMMAND...   (e.g. `smoke.sh spotty`, `smoke.sh flatpak run com.spotty.Spotty`)
# Needs xvfb-run, dbus-run-session and xwininfo. Never run it on a desktop
# you care about: Spotty registers GNOME shortcuts and autostart on start.
set -u
if [ -z "${SMOKE_INNER:-}" ]; then
  export SMOKE_INNER=1 RUST_LOG=spotty=info
  exec xvfb-run -a -s '-screen 0 1280x800x24' dbus-run-session -- sh "$0" "$@"
fi
fail() { echo "SMOKE FAIL: $*"; echo "== log"; tail -40 /tmp/spotty-smoke.log; exit 1; }
settings_windows() { xwininfo -root -tree | grep -c '"Spotty Settings"'; }
# Counted by executable: AppImage and snap processes are named AppRun.wrapped.
running() { for p in /proc/[0-9]*; do readlink "$p/exe"; done 2>/dev/null | grep -c '/spotty$'; }

echo "== first launch: $* (stays in the background)"
"$@" >/tmp/spotty-smoke.log 2>&1 &
i=0
until [ "$(running)" -ge 1 ]; do
  i=$((i + 1)); [ $i -gt 60 ] && fail "Spotty not running 60 s after launch"
  sleep 1
done
sleep 3
[ "$(running)" -eq 1 ] || fail "$(running) Spotty processes after the first launch"
[ "$(settings_windows)" -eq 0 ] || fail "the first launch opened Settings"

echo "== open Settings: $* --settings"
"$@" --settings >>/tmp/spotty-smoke.log 2>&1 &
i=0
until [ "$(settings_windows)" -ge 1 ]; do
  i=$((i + 1)); [ $i -gt 60 ] && fail "no Settings window after 60 s"
  [ "$(running)" -ge 1 ] || fail "Spotty exited"
  sleep 1
done
echo "Settings window open after ${i}s"
sleep 3
[ "$(running)" -eq 1 ] || fail "$(running) Spotty processes after opening Settings"

echo "== second launch (Spotty is already running)"
timeout 30 "$@" --settings >>/tmp/spotty-smoke.log 2>&1
STATUS=$?
[ $STATUS -eq 0 ] || fail "second launch exited with $STATUS"
sleep 2
[ "$(settings_windows)" -eq 1 ] || fail "$(settings_windows) Settings windows after the second launch"
[ "$(running)" -eq 1 ] || fail "$(running) Spotty processes after the second launch"


echo "== quit"
timeout 30 "$@" --quit >>/tmp/spotty-smoke.log 2>&1
i=0
while [ "$(running)" -gt 0 ]; do
  i=$((i + 1)); [ $i -gt 15 ] && fail "Spotty still running 15 s after --quit"
  sleep 1
done
grep -i 'tesseract' /tmp/spotty-smoke.log | head -3
grep -i 'panic' /tmp/spotty-smoke.log && fail "panic in the log"
echo "SMOKE OK"
