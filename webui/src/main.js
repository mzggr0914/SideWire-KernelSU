import { exec, toast } from 'kernelsu';
import './style.css';

const ctl='$(if [ -x /data/adb/modules/sidewire/bin/sidewirectl ]; then printf %s /data/adb/modules/sidewire/bin/sidewirectl; elif [ -x /data/adb/modules_update/sidewire/bin/sidewirectl ]; then printf %s /data/adb/modules_update/sidewire/bin/sidewirectl; else printf %s /data/adb/modules/sidewire/bin/sidewirectl; fi)';
const app=document.querySelector('#app');
app.innerHTML=`
<section class="hero"><div><h1>SideWire</h1><div class="muted">Native Android bridge</div></div><span class="badge" id="state">loading</span></section>
<div class="warning hidden" id="insecureWarning"><b>Insecure mode is active.</b> Authentication and traffic encryption are disabled. Use only on a trusted local network.</div>
<section class="card"><div class="grid">
<div><label>PC host (Outbound only)</label><input id="host" placeholder="192.168.0.150"></div>
<div><label>Port</label><input id="port" type="number" value="58321"></div>
<div><label>Device name</label><input id="name" placeholder="Android"></div>
<div><label>Mode</label><select id="mode"><option value="outbound">Outbound</option><option value="inbound">Inbound</option></select></div>
<div><label>Security</label><select id="security"><option value="secure">Secure (recommended)</option><option value="insecure">Insecure</option></select></div>
<div><label>Autostart</label><select id="autostart"><option value="1">Enabled</option><option value="0">Disabled</option></select></div>
</div><p><button class="primary" id="save">Save configuration</button></p><p class="muted">Restart SideWire after changing connection or security settings.</p></section>
<section class="card"><div class="actions"><button id="start">Start</button><button id="restart">Restart</button><button class="danger" id="stop">Stop</button></div><p class="muted" id="status"></p></section>
`;
app.insertAdjacentHTML('beforeend',`
<section class="card"><div class="card-title"><div><h3>Pairing</h3><div class="muted">Secure mode uses a one-time 6-digit PIN. The desktop server is not required during pairing; paired PCs reconnect automatically.</div></div><div class="actions"><button class="small" id="refreshPaired">Refresh</button><button class="small danger" id="removeAllPaired">Remove all</button></div></div>
<div id="pinBox" class="pin-box hidden"><div class="muted">Pairing PIN</div><div class="pin" id="pin">------</div><div class="muted" id="pinTimer"></div></div>
<p><button class="primary" id="pair">Generate pairing PIN</button></p><div id="paired"></div></section>
<section class="card"><h3>Log</h3><pre id="log">Loading…</pre><button id="refresh">Refresh</button></section>
<div class="modal hidden" id="insecureModal"><div class="modal-card"><h3>Disable SideWire security?</h3><p>This turns off authentication and encryption. Any device on the reachable network may be able to connect to SideWire and request privileged operations.</p><p><b>Only use Insecure mode on a network you fully trust.</b></p><div class="modal-actions"><button id="cancelInsecure">Cancel</button><button class="danger" id="confirmInsecure">I understand, use Insecure</button></div></div></div>
`);

const $=id=>document.getElementById(id);
let loadedSecurity='secure';
let pinTimer=null;
async function sh(cmd){const r=await exec(cmd);if(r.errno!==0)throw new Error(r.stderr||`errno ${r.errno}`);return (r.stdout||'').trim();}
async function get(k,f=''){try{return await sh(`${ctl} get ${k}`)||f}catch{return f}}
function renderSecurity(){const insecure=$('security').value==='insecure';$('insecureWarning').classList.toggle('hidden',!insecure);}
function stripAnsi(text){return text.replace(/\x1B\[[0-?]*[ -\/]*[@-~]/g,'');}
async function refresh(){const st=await sh(`${ctl} status`).catch(e=>`error: ${e.message}`);$('status').textContent=st;$('state').textContent=st.startsWith('running')?'running':'stopped';const log=stripAnsi(await sh(`${ctl} log`).catch(e=>e.message));const view=$('log');view.textContent=log||'No log yet.';requestAnimationFrame(()=>{view.scrollTop=view.scrollHeight;});}
async function refreshPaired(){
  const text=await sh(`${ctl} paired`).catch(()=>"");
  const rows=text?text.split('\n').filter(Boolean):[];
  $('paired').innerHTML=rows.length?'':'<div class="muted">No paired PCs.</div>';
  for(const row of rows){
    const [id,...rest]=row.split('|');
    const name=rest.join('|')||'PC';
    const item=document.createElement('div');item.className='paired-row';
    const label=document.createElement('div');const title=document.createElement('b');title.textContent=name;const short=document.createElement('div');short.className='muted mono';short.textContent=id.slice(0,8);label.append(title,short);
    const remove=document.createElement('button');remove.className='small danger';remove.textContent='Remove';
    remove.onclick=async()=>{try{await sh(`${ctl} unpair ${id}`);toast('Pairing removed');await refreshPaired()}catch(e){toast(e.message)}};
    item.append(label,remove);$('paired').append(item);
  }
}
async function load(){
  $('host').value=await get('host','192.168.0.150');$('port').value=await get('port','58321');$('name').value=await get('name','Android');
  $('mode').value=await get('mode','outbound');$('autostart').value=await get('autostart','0');$('security').value=await get('security','secure');
  loadedSecurity=$('security').value;renderSecurity();await Promise.all([refresh(),refreshPaired()]);
}
async function act(action){try{await sh(`${ctl} ${action}`);toast(`${action} OK`);setTimeout(refresh,400)}catch(e){toast(e.message)}}
async function saveConfig(confirmed=false){
  if($('security').value==='insecure'&&loadedSecurity!=='insecure'&&!confirmed){$('insecureModal').classList.remove('hidden');return;}
  try{
    for(const k of ['host','port','name','mode','security','autostart'])await sh(`${ctl} set ${k} ${JSON.stringify($(k).value)}`);
    loadedSecurity=$('security').value;renderSecurity();toast('Saved. Restart SideWire to apply.');await refresh();
  }catch(e){toast(e.message)}
}
$('save').onclick=()=>saveConfig(false);$('security').onchange=renderSecurity;
$('cancelInsecure').onclick=()=>{$('security').value=loadedSecurity;$('insecureModal').classList.add('hidden');renderSecurity()};
$('confirmInsecure').onclick=()=>{$('insecureModal').classList.add('hidden');saveConfig(true)};
$('pair').onclick=async()=>{try{
  const st=$('status').textContent;if(!st.startsWith('running'))throw new Error('Start the Android SideWire daemon before pairing.');
  const pin=await sh(`${ctl} pair`);if(!/^\d{6}$/.test(pin))throw new Error(`Invalid pairing PIN: ${pin}`);$('pin').textContent=`${pin.slice(0,3)} ${pin.slice(3)}`;$('pinBox').classList.remove('hidden');
  let left=60;$('pinTimer').textContent=`Expires in ${left}s`;clearInterval(pinTimer);pinTimer=setInterval(()=>{left--;if(left<=0){clearInterval(pinTimer);$('pinBox').classList.add('hidden')}else $('pinTimer').textContent=`Expires in ${left}s`;},1000);
  toast('Pairing enabled for 60 seconds');
}catch(e){toast(e.message)}};
$('removeAllPaired').onclick=async()=>{if(!confirm('Remove all paired PCs? They will need a new PIN to reconnect securely.'))return;try{await sh(`${ctl} unpair-all`);toast('All pairings removed');await refreshPaired()}catch(e){toast(e.message)}};
$('start').onclick=()=>act('start');$('stop').onclick=()=>act('stop');$('restart').onclick=()=>act('restart');$('refresh').onclick=refresh;$('refreshPaired').onclick=refreshPaired;
load();
