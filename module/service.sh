#!/system/bin/sh
MODDIR=${0%/*}
CTL="$MODDIR/bin/sidewirectl"
export KSU_MODULE=sidewire
AUTO="$(ksud module config get autostart 2>/dev/null)"
[ "$AUTO" = "1" ] || exit 0
"$CTL" start
