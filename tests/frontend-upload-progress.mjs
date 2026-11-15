import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
const flush = () => new Promise(resolve => setImmediate(resolve));
for (const page of ['index.html', 'demo.html']) {
  const nodes = new Map(), sent = [], timers = new Map(), events = new Map();
  let now = 0, next = 0;
  const element = () => ({value:'',textContent:'',disabled:false,hidden:true,listeners:{},classList:{add(){},remove(){},toggle(){}},append(){},replaceChildren(){},setAttribute(){},contains(){return false;},addEventListener(k,f){this.listeners[k]=f;}});
  const get = id => {if(!nodes.has(id))nodes.set(id,element());return nodes.get(id);};
  class XHR {
    constructor(){this.upload={};}
    open(method,url){this.method=method;this.url=url;}
    setRequestHeader(){}
    send(body){this.body=body;sent.push(this);}
    abort(){this.onabort();}
  }
  const ctx = vm.createContext({document:{getElementById:get,createElement:element,createDocumentFragment:element},
    window:{addEventListener(k,f){events.set(k,f);}},location:{search:'',pathname:'/'},history:{pushState(){}},URL,URLSearchParams,TextEncoder,
    performance:{now:()=>now},setInterval(f){timers.set(++next,f);return next;},clearInterval(i){timers.delete(i);},XMLHttpRequest:XHR,
    setTimeout(f){f();return 1;},
    fetch:async(url,opts={})=>({ok:true,status:opts.method==='HEAD'?404:200,json:async()=>({entries:[]})})});
  const script=readFileSync(new URL('../frontend/'+page,import.meta.url),'utf8').match(/<script>([\s\S]*?)<\/script>/)[1];
  vm.runInContext(script.slice(script.indexOf('const $')),ctx);await flush();
  const backgroundTimers=timers.size; // Photoframe status poll is independent of upload lifetime.
  const start = files => get('picker').onchange({target:{files}});
  start([{name:'BIG.BIN',size:1024**3}]);
  const x=sent[0];assert.equal(x.method,'PUT');assert.equal(x.timeout,0);
  assert.equal(get('upload-panel').hidden,false);assert.equal(get('upload-stop').disabled,false);
  now=10000;x.upload.onprogress({loaded:10*1024**2,total:1024**3,lengthComputable:true});
  assert.equal(get('upload-percent').textContent,'1.0%');
  assert.match(get('upload-speed').textContent,/1\.00 МиБ\/с/);
  assert.match(get('upload-time').textContent,/16 мин 54 с/);
  assert.match(get('upload-bytes').textContent,/10\.00 МиБ \/ 1\.00 ГиБ/);
  now=15000;for(const f of timers.values())f();assert.match(get('upload-phase').textContent,/Нет новых данных 5 с/);
  x.upload.onload();assert.match(get('upload-phase').textContent,/Ожидаем подтверждение/);
  assert.equal(vm.runInContext('uploadRunning',ctx),true);
  let prevented=false;events.get('beforeunload')({preventDefault(){prevented=true;}});assert(prevented);
  x.status=201;x.onload();await flush();
  assert.match(get('upload-phase').textContent,/сохранён на SD/);assert.equal(timers.size,backgroundTimers);
  assert.equal(get('refresh').disabled,false);
  start([{name:'CANCEL.BIN',size:1000},{name:'QUEUED.BIN',size:1000}]);
  assert.equal(sent.length,2);get('upload-stop').onclick();await flush();
  assert.equal(sent.length,2);assert.equal(vm.runInContext('uploadQueue.length',ctx),0);
  assert.match(get('status').textContent,/Отменено: 1/);assert.match(get('upload-phase').textContent,/отменена/);
  assert.equal(timers.size,backgroundTimers);assert.equal(get('refresh').disabled,false);
  start([{name:'EXISTS.BIN',size:1000}]);sent[2].status=409;sent[2].onload();await flush();
  assert.match(get('upload-phase').textContent,/409.*имя уже занято/);assert.equal(timers.size,backgroundTimers);
  start([{name:'BROKEN.BIN',size:1000}]);sent[3].onerror();await flush();
  assert.match(get('upload-phase').textContent,/соединение.*прервалось/);assert.equal(timers.size,backgroundTimers);
  start([{name:'EMPTY.BIN',size:0}]);sent[4].upload.onload();sent[4].status=201;sent[4].onload();await flush();
  assert.equal(get('upload-percent').textContent,'100.0%');assert.equal(timers.size,backgroundTimers);
  console.log(page+': progress, speed/ETA, stall, SD confirmation, cancellation/queue clear, errors, zero-length and timer cleanup OK');
}
