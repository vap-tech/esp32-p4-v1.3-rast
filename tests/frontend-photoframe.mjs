import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
const flush=()=>new Promise(r=>setImmediate(r));
for(const page of ['index.html','demo.html']) {
 const nodes=new Map(),calls=[];
 const el=()=>({value:'',checked:false,disabled:false,hidden:false,textContent:'',classList:{add(){},remove(){},toggle(){}},append(){},replaceChildren(){},setAttribute(){},addEventListener(){},showModal(){this.open=true;},close(){this.open=false;}});
 const get=id=>{if(!nodes.has(id))nodes.set(id,el());return nodes.get(id);};
 let fail=false;
 const ctx=vm.createContext({document:{getElementById:get,createElement:el,createDocumentFragment:el},window:{addEventListener(){}},location:{pathname:'/',search:''},history:{pushState(){}},URL,URLSearchParams,TextEncoder,
 setInterval:()=>1,clearInterval(){},fetch:async(url,opts={})=>{calls.push({url,opts});return {ok:!fail,status:fail?409:200,json:async()=>url.startsWith('/api/photoframe')?(fail?{error:'alarm active'}:{running:url.includes('/start'),folder:'/PHOTOS',shown:1,skipped:0}):{entries:[]}};}});
 const script=readFileSync(new URL('../frontend/'+page,import.meta.url),'utf8').match(/<script>([\s\S]*?)<\/script>/)[1];
 vm.runInContext(script.slice(script.indexOf('const $')),ctx);await flush();calls.length=0;
 vm.runInContext('current="/PHOTOS/Тест"',ctx);get('photo-open').onclick();
 assert.equal(get('photo-dialog').open,true);assert.match(get('photo-folder').textContent,/Тест/);
 // Capture the selected folder when opening, regardless of subsequent navigation.
 vm.runInContext('current="/OTHER"',ctx);
 get('photo-interval').value='10';get('photo-duration').value='';get('photo-fit').value='0';get('photo-wipe').checked=true;get('photo-slide').checked=true;
 await get('photo-form').onsubmit({preventDefault(){}});
 assert.equal(calls[0].opts.method,'POST');assert.equal(calls[0].url,'/api/photoframe/start?path=%2FPHOTOS%2F%D0%A2%D0%B5%D1%81%D1%82&options=10,0,0,12');
 assert.equal(get('photo-dialog').open,false);assert.equal(get('photo-stop').hidden,false);
 await get('photo-stop').onclick();assert.equal(calls.at(-1).url,'/api/photoframe/stop');assert.equal(get('photo-stop').hidden,true);
 get('photo-open').onclick();get('photo-duration').value='3';get('photo-mirror').checked=true;
 fail=true;await get('photo-form').onsubmit({preventDefault(){}});
 assert.match(calls.at(-1).url,/options=10,180,0,13/);assert.equal(get('photo-dialog').open,true);assert.match(get('photo-error').textContent,/alarm active/);
 const n=calls.length;get('photo-interval').value='1';await get('photo-form').onsubmit({preventDefault(){}});assert.equal(calls.length,n);
 console.log(page+': photoframe captured folder, effects, duration, explicit stop and unconfirmed start OK');
}
