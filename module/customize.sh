#!/system/bin/sh
ui_print "- SideWire 1.0.0"
ui_print "- Root/shell execution + KernelSU WebUI"
[ "$ARCH" = "arm64" ] || abort "SideWire currently supports arm64 only"
set_perm "$MODPATH/bin/sidewired" 0 0 0755
set_perm "$MODPATH/bin/sidewirectl" 0 0 0755
set_perm "$MODPATH/bin/sidewire-clipboard.jar" 0 0 0644
set_perm "$MODPATH/service.sh" 0 0 0755
set_perm "$MODPATH/action.sh" 0 0 0755
set_perm "$MODPATH/sepolicy.rule" 0 0 0644
export KSU_MODULE=sidewire
for pid in $(pidof sidewired 2>/dev/null); do
  [ "$(cat "/proc/$pid/comm" 2>/dev/null)" = "sidewired" ] && kill "$pid" 2>/dev/null
done
sleep 1
rm -f /data/adb/sidewire/sidewired.pid /data/adb/modules/sidewire/sidewired.pid
STATE_DIR="/data/adb/sidewire"
PAIRS_DIR="$STATE_DIR/paired_hosts"
OLD_PAIRS="/data/adb/modules/sidewire/config/paired_hosts"
mkdir -p "$STATE_DIR" "$PAIRS_DIR"
chmod 0700 "$STATE_DIR" "$PAIRS_DIR" 2>/dev/null
if [ -d "$OLD_PAIRS" ]; then
  for old in "$OLD_PAIRS"/*.pair; do
    [ -f "$old" ] || continue
    dest="$PAIRS_DIR/${old##*/}"
    [ -f "$dest" ] || cp "$old" "$dest" 2>/dev/null
  done
fi
chmod 0600 "$PAIRS_DIR"/*.pair 2>/dev/null
[ -n "$(ksud module config get mode 2>/dev/null)" ] || ksud module config set mode outbound >/dev/null 2>&1
[ -n "$(ksud module config get port 2>/dev/null)" ] || ksud module config set port 58321 >/dev/null 2>&1
[ -n "$(ksud module config get autostart 2>/dev/null)" ] || ksud module config set autostart 0 >/dev/null 2>&1
[ -n "$(ksud module config get security 2>/dev/null)" ] || ksud module config set security secure >/dev/null 2>&1
[ -n "$(ksud module config get pairing_port 2>/dev/null)" ] || ksud module config set pairing_port 58323 >/dev/null 2>&1
if [ -z "$(ksud module config get device_id 2>/dev/null)" ]; then
  device_id="$(cat /proc/sys/kernel/random/uuid 2>/dev/null | tr -d '-')"
  [ -n "$device_id" ] && ksud module config set device_id "$device_id" >/dev/null 2>&1
fi
ui_print "- Open the module WebUI to configure the PC endpoint"
