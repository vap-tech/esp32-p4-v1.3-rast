import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
const flush=()=>new Promise(r=>setImmediate(r));
for(const page of ['index.html','demo.html']) {
 const nodes=new Map();
 const el=()=>({value:'',hidden:false,disabled:false,textContent:'',children:[],attrs:new Map(),classList:{add(){},remove(){},toggle(){}},append(...x){this.children.push(...x)},replaceChildren(){this.children=[]},setAttribute(k,v){this.attrs.set(k,v)},addEventListener(){}});
 const get=id=>{if(!nodes.has(id))nodes.set(id,el());return nodes.get(id)};
 const pendingIds=new Set(['E00000001']);let failAck=true,finishAck,ackPath;
 const ctx=vm.createContext({document:{getElementById:get,createElement:el,createDocumentFragment:el},window:{addEventListener(){}},location:{pathname:'/',search:''},history:{pushState(){}},URL,URLSearchParams,TextEncoder,setInterval(){},clearInterval(){},
 fetch:async(url,opts={})=>{
  const reply=(status,data)=>({ok:status<300,status,json:async()=>data});
  if(url.startsWith('/api/photoframe'))return reply(200,{running:false,message:'idle'});
  if(url.startsWith('/api/errors?cursor='))return reply(200,{entries:Array.from(pendingIds,x=>x+'.JSON'),next_cursor:null});
  if(url.endsWith('.JSON'))return reply(200,{title:'Пропуск фотографии',file:'/test.jpg',code:'JPEG hardware error',detail:'registers'});
  if(opts.method==='DELETE') {ackPath=url;assert.equal(opts.body,undefined);if(failAck)return reply(503);return new Promise(resolve=>{finishAck=()=>{pendingIds.delete('E00000001');resolve(reply(201))}})}
  return reply(200,{entries:[]});
 }});
 const raw=readFileSync(new URL('../frontend/'+page,import.meta.url),'utf8').match(/<script>([\s\S]*?)<\/script>/)[1];vm.runInContext(raw.slice(raw.indexOf('const $')),ctx);await flush();
 assert.equal(get('error-journal').hidden,false);assert.equal(get('error-journal-list').children.length,1);
 let card=get('error-journal-list').children[0];assert.equal(card.children[0].children[2].href,'/fs/SYSTEM/ERRORS/E00000001.JSON');assert.equal(pendingIds.size,1);
 await card.children[1].onclick();assert.equal(get('error-journal-list').children.length,1);assert.equal(pendingIds.size,1);
 failAck=false;const pending=card.children[1].onclick();await flush();assert.equal(get('error-journal-list').children.length,1);finishAck();await pending;
 assert.equal(ackPath,'/fs/SYSTEM/ERRORS/UNREAD/E00000001.SEEN');assert.equal(get('error-journal').hidden,true);
 await vm.runInContext('pollErrorJournal()',ctx);assert.equal(get('error-journal').hidden,true);
 // New browser state must respect the SD marker, not a local dismissal cache.
 vm.runInContext('errorReports.clear()',ctx);await vm.runInContext('pollErrorJournal()',ctx);assert.equal(get('error-journal-list').children.length,0);
 console.log(page+': persistent unread reports, independent detail link and confirmed acknowledgement OK');
}
