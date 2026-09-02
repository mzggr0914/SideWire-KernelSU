pub const CORE: u64 = 1 << 0;
pub const PTY_COMPLETION: u64 = 1 << 1;
pub const SECURE_PROXY: u64 = 1 << 2;
pub const CLIPBOARD: u64 = 1 << 3;

pub const ALL: u64 = CORE | PTY_COMPLETION | SECURE_PROXY | CLIPBOARD;

pub fn names(capabilities: u64) -> Vec<&'static str> {
    let mut out = Vec::new();
    for (bit, name) in [
        (CORE, "core"),
        (PTY_COMPLETION, "pty-completion"),
        (SECURE_PROXY, "secure-proxy"),
        (CLIPBOARD, "clipboard"),
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
