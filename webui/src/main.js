import { exec, toast } from 'kernelsu';
import './style.css';

const ctl='/data/adb/modules/sidewire/bin/sidewirectl';
const app=document.querySelector('#app');
app.innerHTML=`<section class="hero"><div><h1>SideWire</h1><div class="muted">Native Android bridge</div></div><span class="badge" id="state">loading</span></section><section class="card"><div class="grid"><div><label>PC host (Outbound only)</label><input id="host" placeholder="192.168.0.150"></div><div><label>Port</label><input id="port" type="number" value="58321"></div><div><label>Device name</label><input id="name" placeholder="A32"></div><div><label>Mode</label><select id="mode"><option value="outbound">Outbound</option><option value="inbound">Inbound</option></select></div><div><label>Autostart</label><select id="autostart"><option value="1">Enabled</option><option value="0">Disabled</option></select></div></div><p><button class="primary" id="save">Save configuration</button></p></section><section class="card"><div class="actions"><button id="start">Start</button><button id="restart">Restart</button><button class="danger" id="stop">Stop</button></div><p class="muted" id="status"></p></section><section class="card"><h3>Log</h3><pre id="log">Loading…</pre><button id="refresh">Refresh</button></section>`;
const $=id=>document.getElementById(id);
async function sh(cmd){const r=await exec(cmd);if(r.errno!==0)throw new Error(r.stderr||`errno ${r.errno}`);return (r.stdout||'').trim();}
async function get(k,f=''){try{return await sh(`${ctl} get ${k}`)||f}catch{return f}}
async function refresh(){const st=await sh(`${ctl} status`).catch(e=>`error: ${e.message}`);$('status').textContent=st;$('state').textContent=st.startsWith('running')?'running':'stopped';$('log').textContent=await sh(`${ctl} log`).catch(e=>e.message);}
async function load(){$('host').value=await get('host','192.168.0.150');$('port').value=await get('port','58321');$('name').value=await get('name','Android');$('mode').value=await get('mode','outbound');$('autostart').value=await get('autostart','0');await refresh();}
async function act(action){try{await sh(`${ctl} ${action}`);toast(`${action} OK`);setTimeout(refresh,400)}catch(e){toast(e.message)}}
$('save').onclick=async()=>{try{for(const k of ['host','port','name','mode','autostart'])await sh(`${ctl} set ${k} ${JSON.stringify($(k).value)}`);toast('Saved');await refresh()}catch(e){toast(e.message)}};
$('start').onclick=()=>act('start');$('stop').onclick=()=>act('stop');$('restart').onclick=()=>act('restart');$('refresh').onclick=refresh;load();
