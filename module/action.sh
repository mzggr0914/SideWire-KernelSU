#!/system/bin/sh
MODDIR=${0%/*}
CTL="$MODDIR/bin/sidewirectl"
STATE_DIR="/data/adb/sidewire"
ACTION_FILE="$STATE_DIR/action"
export KSU_MODULE=sidewire

action="$(cat "$ACTION_FILE" 2>/dev/null)"
rm -f "$ACTION_FILE"
case "$action" in
  start) "$CTL" _start;;
  stop) "$CTL" _stop;;
  restart) "$CTL" _restart;;
  *) echo "unknown SideWire action: $action" >&2; exit 2;;
esac