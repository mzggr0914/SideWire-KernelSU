import fs from 'node:fs';
const file='apps/sidewire/src/main.rs';
let s=fs.readFileSync(file,'utf8');
if(!s.includes('async fn remote_push(')) {
  const marker='\nasync fn shell_exec(';
  const at=s.indexOf(marker);
  if(at<0) throw new Error('shell_exec marker not found');
  s=s.slice(0,at)+'\n'+fs.readFileSync('scripts/v040-cli-helpers.txt','utf8')+s.slice(at);
}
fs.writeFileSync(file,s);
