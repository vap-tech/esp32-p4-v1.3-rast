import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
const flush=()=>new Promise(r=>setImmediate(r));
for(const page of ['index.html','demo.html']) {
 const nodes=new Map(),calls=[];
 const element=()=>({textContent:'',value:'',disabled:false,classList:{add(){},remove(){},toggle(){}},append(){},replaceChildren(){},setAttribute(){},addEventListener(){}});
 const get=id=>{if(!nodes.has(id))nodes.set(id,element());return nodes.get(id);};
 let confirmed=false,code=200,payload={freed_bytes:5875040,psram_used:0,open_files:0},pending;
 const ctx=vm.createContext({document:{getElementById:get,createElement:element,createDocumentFragment:element},window:{addEventListener(){}},location:{pathname:'/',search:''},history:{pushState(){}},URL,URLSearchParams,TextEncoder,confirm:()=>confirmed,
 fetch:async(url,options={})=>{
  calls.push({url,options});
  if(url==='/api/resources/release') {
   if(pending)await new Promise(resolve=>{pending=resolve;});
   return {ok:code===200,status:code,json:async()=>payload};
  }
  return {ok:true,status:200,json:async()=>({entries:[]})};
 }});
 const s=readFileSync(new URL('../frontend/'+page,import.meta.url),'utf8').match(/<script>([\s\S]*?)<\/script>/)[1];vm.runInContext(s.slice(s.indexOf('const $')),ctx);await flush();calls.length=0;
 await get('release-resources').onclick();assert.equal(calls.length,0);
 confirmed=true;pending=true;const running=get('release-resources').onclick();await flush();
 assert.equal(get('release-resources').disabled,true);assert.match(get('release-status').textContent,/Закрытие/);
 vm.runInContext('queueJobs([{file:{name:"NO.TXT",size:1},dir:"/",relative:"NO.TXT"}])',ctx);assert.match(get('status').textContent,/освобождения/);
 const resolve=pending;pending=null;resolve();await running;
 assert.equal(calls[0].url,'/api/resources/release');assert.equal(calls[0].options.method,'POST');assert.match(get('release-status').textContent,/Веб работает/);assert.equal(get('release-resources').disabled,false);
 code=409;await get('release-resources').onclick();assert.match(get('release-status').textContent,/будильник/);
 code=200;payload={freed_bytes:0,psram_used:1,open_files:0};await get('release-resources').onclick();assert.match(get('release-status').textContent,/не подтвердила/);
}
console.log('resource UI: confirmation, pending, upload lock, completion, alarm conflict and incomplete reset OK');
