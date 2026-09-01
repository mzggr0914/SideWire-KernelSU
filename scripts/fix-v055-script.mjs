import fs from "node:fs";
const path = "scripts/patch-v055-pty-duplex.mjs";
let text = fs.readFileSync(path, "utf8");
const bad = ") -> Result<(tokio::fs::File, tokio::fs::File, StdFile, std::ffi::CString)> {\n`;\n";
const good = ") -> Result<(tokio::fs::File, tokio::fs::File, StdFile, std::ffi::CString)> {\n";
if (!text.includes(bad)) throw new Error("bad template boundary not found");
text = text.replace(bad, good);
fs.writeFileSync(path, text);
