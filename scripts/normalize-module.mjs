import fs from 'node:fs';
for (const p of [
  'module/module.prop',
  'module/customize.sh',
  'module/service.sh',
  'module/bin/sidewirectl',
  'module/sepolicy.rule',
]) {
  const s = fs.readFileSync(p, 'utf8').replace(/\r\n/g, '\n');
  fs.writeFileSync(p, s);
}
