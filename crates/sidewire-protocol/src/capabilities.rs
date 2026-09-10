pub const CORE: u64 = 1 << 0;
pub const PTY_COMPLETION: u64 = 1 << 1;
pub const SECURE_PROXY: u64 = 1 << 2;
pub const CLIPBOARD: u64 = 1 << 3;
pub const HEARTBEAT: u64 = 1 << 4;
pub const STREAM_CANCEL: u64 = 1 << 5;
pub const FLOW_CONTROL: u64 = 1 << 6;

pub const ALL: u64 =
    CORE | PTY_COMPLETION | SECURE_PROXY | CLIPBOARD | HEARTBEAT | STREAM_CANCEL | FLOW_CONTROL;

pub fn names(capabilities: u64) -> Vec<&'static str> {
    let mut out = Vec::new();
    for (bit, name) in [
        (CORE, "core"),
        (PTY_COMPLETION, "pty-completion"),
        (SECURE_PROXY, "secure-proxy"),
        (CLIPBOARD, "clipboard"),
        (HEARTBEAT, "heartbeat"),
        (STREAM_CANCEL, "stream-cancel"),
        (FLOW_CONTROL, "flow-control"),
    ] {
        if capabilities & bit != 0 {
            out.push(name);
        }
    }
    out
}

pub fn display(capabilities: u64) -> String {
    let names = names(capabilities);
    if names.is_empty() {
        "none".into()
    } else {
        names.join(",")
    }
}
