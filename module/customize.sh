#!/system/bin/sh
ui_print "- SideWire 0.6.1"
ui_print "- Root/shell execution + KernelSU WebUI"
[ "$ARCH" = "arm64" ] || abort "SideWire currently supports arm64 only"
set_perm "$MODPATH/bin/sidewired" 0 0 0755
set_perm "$MODPATH/bin/sidewirectl" 0 0 0755
set_perm "$MODPATH/service.sh" 0 0 0755
set_perm "$MODPATH/sepolicy.rule" 0 0 0644
export KSU_MODULE=sidewire
[ -n "$(ksud module config get mode 2>/dev/null)" ] || ksud module config set mode outbound >/dev/null 2>&1
[ -n "$(ksud module config get port 2>/dev/null)" ] || ksud module config set port 58321 >/dev/null 2>&1
[ -n "$(ksud module config get autostart 2>/dev/null)" ] || ksud module config set autostart 0 >/dev/null 2>&1
ui_print "- Open the module WebUI to configure the PC endpoint"
