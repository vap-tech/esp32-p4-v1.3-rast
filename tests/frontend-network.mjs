import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
const nodes = new Map(), requests = [];
const element = () => ({value:'',textContent:'',disabled:false,children:[],style:{},classList:{add(){},remove(){},toggle(){}},setAttribute(){},append(){},replaceChildren(){},addEventListener(){}});
const get = id => {if(!nodes.has(id))nodes.set(id,element());return nodes.get(id);};
let broken = false;
const ctx = vm.createContext({document:{getElementById:get,createElement:element,createDocumentFragment:element},window:{addEventListener(){}},location:{search:'',pathname:'/'},history:{pushState(){}},URL,URLSearchParams,TextEncoder,AbortController,performance,
 fetch:async(url,opts={})=>{
  if (!url.startsWith('/api/net-test')) return new Response(JSON.stringify({entries:[]}));
  requests.push({url,opts});
  if(opts.method==='POST')return new Response(JSON.stringify({received:opts.body.length}));
  const data=Uint8Array.from({length:1025},(_,i)=>i&255);if(broken)data[512]^=1;
  return new Response(new ReadableStream({start(c){c.enqueue(data.slice(0,333));c.enqueue(data.slice(333));c.close();}}));
 }});
const script=readFileSync(new URL('../frontend/index.html',import.meta.url),'utf8').match(/<script>([\s\S]*?)<\/script>/)[1];
vm.runInContext(script,ctx);
await new Promise(r=>setImmediate(r));
get('net-size').value='1025';
await get('net-test').onclick();
assert.equal(requests.length,2);assert.equal(requests[1].opts.body.length,1025);
assert.match(get('net-status').textContent,/Борда → браузер/);assert.match(get('net-status').textContent,/Браузер → борда/);
assert.match(get('net-status').textContent,/SD не использовалась/);assert.equal(get('net-test').disabled,false);assert.equal(get('net-stop').disabled,true);
requests.length=0;broken=true;
await get('net-test').onclick();
assert.equal(requests.length,1);assert.match(get('net-status').textContent,/повреждены/);assert.equal(get('net-test').disabled,false);
console.log('network UI: streamed download validation, upload count, directions, corrupt data and reset OK');
