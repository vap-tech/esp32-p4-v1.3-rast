import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
const tick=()=>new Promise(r=>setImmediate(r));
const enc=new TextEncoder();
for(const page of ['index.html','demo.html']){
 const nodes=new Map(),calls=[],prompts=[];let holdPut,releasePut,failPublish=false,random=1;
 const el=()=>({value:'',textContent:'',disabled:false,open:false,hidden:false,children:[],classList:{add(){},remove(){},toggle(){}},append(...v){this.children.push(...v)},replaceChildren(...v){this.children=v},setAttribute(){},addEventListener(){},focus(){},showModal(){this.open=true},close(){this.open=false;this.onclose?.()}});
 const get=id=>{if(!nodes.has(id))nodes.set(id,el());return nodes.get(id)};
 const original=enc.encode('\uFEFFстрока 1\r\nстрока 2\r\n');
 const disk=new Map([['/DOCS/CONFIG.TXT',original],['/DOCS/TAKEN.TXT',enc.encode('existing')],['/DOCS/BINARY.BIN',new Uint8Array([255,0,1])],['/DOCS/LIMIT.TXT',enc.encode('a'.repeat(1048576))]]);
 const response=(status,bytes=new Uint8Array())=>({status,ok:status>=200&&status<300,headers:{get:()=>String(bytes.length)},arrayBuffer:async()=>bytes.slice().buffer,json:async()=>({entries:[]})});
 const fetch=async(url,opts={})=>{
  const method=opts.method||'GET';calls.push({url,method,body:opts.body});
  const u=new URL(url,'http://board');
  if(u.pathname==='/api/rename'){
   const path=u.searchParams.get('path'),name=u.searchParams.get('name'),dest=path.slice(0,path.lastIndexOf('/')+1)+name;
   if(!disk.has(path))return response(404);if(disk.has(dest))return response(409);
   if(failPublish&&path.endsWith('.TMP'))return response(503);
   disk.set(dest,disk.get(path));disk.delete(path);return response(204);
  }
  if(u.pathname.startsWith('/fs/')){
   const path=decodeURIComponent(u.pathname.slice(3));
   if(method==='GET')return disk.has(path)?response(200,disk.get(path)):response(404);
   if(method==='DELETE'){if(!disk.has(path))return response(404);disk.delete(path);return response(204)}
   if(method==='PUT'){
    if(disk.has(path))return response(409);
    if(holdPut){holdPut=false;await new Promise(r=>releasePut=r)}
    disk.set(path,new Uint8Array(opts.body));return response(201);
   }
  }
  if(u.pathname==='/api/photoframe')return {...response(200),json:async()=>({running:false,message:'idle'})};
  return response(200);
 };
 const ctx=vm.createContext({document:{getElementById:get,createElement:el,createDocumentFragment:el,addEventListener(){}},window:{addEventListener(){}},location:{pathname:'/',search:''},history:{pushState(){}},URL,URLSearchParams,TextEncoder,TextDecoder,Uint8Array,Uint32Array,crypto:{getRandomValues(a){a.fill(random++);return a}},prompt:()=>prompts.shift(),fetch,setInterval(){},clearInterval(){}});
 const raw=readFileSync(new URL('../frontend/'+page,import.meta.url),'utf8').match(/<script>([\s\S]*?)<\/script>/)[1];vm.runInContext(raw.slice(raw.indexOf('const $')),ctx);await tick();calls.length=0;
 await vm.runInContext('openEditor("/DOCS/CONFIG.TXT")',ctx);
 assert.equal(get('editor-text').value,'строка 1\nстрока 2\n');
 // An unchanged save preserves the original bytes, including BOM/CRLF.
 await vm.runInContext('saveEditor()',ctx);assert.deepEqual(disk.get('/DOCS/CONFIG.TXT'),original);
 assert.deepEqual(calls.filter(c=>c.method!=='GET').map(c=>c.method),['PUT','DELETE','POST']);
 get('editor-text').value='новый\nтекст\n';calls.length=0;holdPut=true;
 const save=vm.runInContext('saveEditor()',ctx);await tick();assert.equal(calls.filter(c=>c.method==='POST'||c.method==='DELETE').length,0);assert.equal(get('editor-text').disabled,true);
 // Folder navigation cannot redirect an already opened editor's save.
 vm.runInContext('current="/OTHER"',ctx);releasePut();await save;
 assert.deepEqual(disk.get('/DOCS/CONFIG.TXT'),enc.encode('\uFEFFновый\r\nтекст\r\n'));
 const saved=disk.get('/DOCS/CONFIG.TXT').slice();
 prompts.push('TAKEN.TXT');await vm.runInContext('saveEditor(true)',ctx);assert.deepEqual(disk.get('/DOCS/TAKEN.TXT'),enc.encode('existing'));assert.deepEqual(disk.get('/DOCS/CONFIG.TXT'),saved);assert.match(get('editor-message').textContent,/409/);
 prompts.push('COPY.TXT');await vm.runInContext('saveEditor(true)',ctx);assert.deepEqual(disk.get('/DOCS/COPY.TXT'),saved);assert.deepEqual(disk.get('/DOCS/CONFIG.TXT'),saved);assert.equal(get('editor-path').textContent,'/DOCS/COPY.TXT');
 // Failure after DELETE may lose the original; retain the uploaded temp and permit retry.
 failPublish=true;get('editor-text').value='not published';await vm.runInContext('saveEditor()',ctx);failPublish=false;assert.equal(disk.has('/DOCS/COPY.TXT'),false);assert.equal([...disk.keys()].filter(k=>k.includes('.EDIT-')).length,1);assert.equal(vm.runInContext('mutating',ctx),false);
 await vm.runInContext('saveEditor()',ctx);assert.deepEqual(disk.get('/DOCS/COPY.TXT'),enc.encode('\uFEFFnot published'));
 calls.length=0;await vm.runInContext('openEditor("/TOO_BIG",{size:1048577})',ctx);assert.equal(calls.length,0);assert.match(get('editor-message').textContent,/1 МиБ/);
 await vm.runInContext('openEditor("/DOCS/LIMIT.TXT")',ctx);assert.equal(get('editor-save').disabled,false);get('editor-text').value+='я';get('editor-text').oninput();assert.equal(get('editor-save').disabled,true);
 await vm.runInContext('openEditor("/DOCS/BINARY.BIN")',ctx);assert.equal(get('editor-save').disabled,true);assert.match(get('editor-message').textContent,/UTF-8|двоичные/);
 await vm.runInContext('openEditor("/DOCS/CONFIG.TXT")',ctx);get('editor-text').value='unsaved';get('editor-close').onclick();assert.equal(get('editor-dialog').open,false);assert.deepEqual(disk.get('/DOCS/CONFIG.TXT'),saved);
 // Incomplete streaming reads must stop at the same limit, independent of headers.
 let cancelled=false;ctx.large={headers:{get:()=>null},body:{getReader:()=>({read:async()=>({value:new Uint8Array(1048577),done:false}),cancel:async()=>{cancelled=true},releaseLock(){}})}};
 await assert.rejects(vm.runInContext('readEditorBytes(large)',ctx));assert.equal(cancelled,true);
 assert.equal([...disk.keys()].filter(k=>k.includes('.EDIT-')).length,1);assert(![...disk.keys()].some(k=>k.endsWith('.BAK')));
 console.log(page+': editor byte limit, UTF-8/BOM/newlines, PUT confirmation, Save As conflicts, rename failure and silent close OK');
}
