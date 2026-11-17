// Functional checks for the shared browser app; no npm dependencies.
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
import {xhrAdapter} from './frontend-xhr.mjs';
const flush = () => new Promise(r => setImmediate(r));
for (const page of ['index.html', 'demo.html']) {
  const nodes = new Map(), calls = [], pending = [], opens = [];
  const element = tag => ({tag, children: [], value: '', textContent: '', disabled: false, listeners: {},
    classList: {add(){}, remove(){}, toggle(){}}, setAttribute(){}, contains(){return false;},
    append(...items){this.children.push(...items);}, replaceChildren(...items){this.children = items;},
    addEventListener(k,v){this.listeners[k]=v;}, click(){opens.push(this.href);}});
  const get = id => {if(!nodes.has(id))nodes.set(id,element(id));return nodes.get(id);};
  let nextName = 'NEW.TXT';
  const ctx = vm.createContext({document:{getElementById:get,createElement:element,createDocumentFragment:()=>element('fragment')},
    window:{addEventListener(){},open(url){opens.push(url);}}, location:{search:'',pathname:'/'}, history:{pushState(){}},
    URL, URLSearchParams, TextEncoder, prompt:()=>nextName, confirm:()=>true,
    fetch(url,opts={}) {
      calls.push({url,opts});
      if(opts.method==='PUT')return new Promise(resolve=>pending.push(()=>resolve({ok:true,status:201})));
      return Promise.resolve({ok:true,status:opts.method==='MKCOL'?201:200,json:async()=>({entries:[]}),text:async()=>''});
    }});
  ctx.XMLHttpRequest = xhrAdapter((...args) => ctx.fetch(...args));
  ctx.performance = {now: () => 0};
  ctx.setInterval = () => 1; ctx.clearInterval = () => {};
  const raw = readFileSync(new URL('../frontend/'+page,import.meta.url),'utf8').match(/<script>([\s\S]*?)<\/script>/)[1];
  vm.runInContext(raw.slice(raw.indexOf('const $')),ctx);await flush();
  vm.runInContext('render([{name:"PIC.JPG",dir:false,size:10}])',ctx);
  const row = get('files').children[0].children[0];
  const name = row.children[0].children[1].children[0], menu=row.children.find(e=>e.className==='file-actions');
  assert.equal(name.tag,'a');assert.equal(name.href,'/fs/PIC.JPG');assert.equal(name.target,'_blank');
  assert.equal(menu.children[0].tag,'button');
  const popup=menu.children[1]; popup.hidePopover=()=>{};
  assert.deepEqual(popup.children.map(o=>o.textContent),['Открыть','Скачать','Переименовать','Удалить']);
  popup.children[1].onclick();assert.equal(opens.pop(),'/fs/PIC.JPG?download=1');
  popup.children[2].onclick();await flush();
  assert(calls.some(c=>c.opts.method==='POST'&&c.url==='/api/rename?path=%2FPIC.JPG&name=NEW.TXT'));
  popup.children[3].onclick();await flush();assert(calls.some(c=>c.opts.method==='DELETE'&&c.url==='/fs/PIC.JPG'));
  calls.length=0;
  get('folderpicker').onchange({target:{files:[{name:'A.TXT',size:1,webkitRelativePath:'DOCS/SUB/A.TXT'},{name:'B.TXT',size:1,webkitRelativePath:'DOCS/B.TXT'}]}});
  await flush();
  assert.deepEqual(calls.slice(0,3).map(c=>[c.opts.method,c.url]),[['MKCOL','/fs/DOCS'],['MKCOL','/fs/DOCS/SUB'],['PUT','/fs/DOCS/SUB/A.TXT']]);
  assert.equal(pending.length,1);
  vm.runInContext('current="/OTHER"',ctx);
  pending.shift()();await flush();
  assert(calls.some(c=>c.opts.method==='PUT'&&c.url==='/fs/DOCS/B.TXT')); // original destination
  pending.shift()();await flush();
  // Recursively deleting a folder performs child deletes before parent.
  ctx.fetch = async(url,opts={})=>{calls.push({url,opts});return {ok:true,status:200,json:async()=>({entries:url.includes('path=%2FTREE')?[{name:'A.TXT',dir:false}]:[]})};};
  calls.length=0;
  await vm.runInContext('removeTree("/TREE",true)',ctx);
  assert.deepEqual(calls.map(c=>[c.opts.method,c.url]),[['GET','/api/list?path=%2FTREE'],['DELETE','/fs/TREE/A.TXT'],['DELETE','/fs/TREE']]);
  // A DELETE keeps a visible waiting message until the board confirms completion.
  let completeDelete, timers = 0;
  ctx.setInterval = () => { timers++; return 123; };
  ctx.clearInterval = () => { timers--; };
  ctx.fetch = (url, opts={}) => new Promise(resolve => { completeDelete = resolve; });
  const deletion = vm.runInContext('deleteSelection("/BIG.MKV",false)',ctx);
  await flush();
  assert.equal(get('delete-status').hidden, false);
  assert.match(get('delete-status').textContent, /Идёт удаление: \/BIG.MKV/);
  assert.doesNotMatch(get('delete-status').textContent, /^Удалено/);
  assert.equal(timers, 1);
  vm.runInContext('current="/OTHER"',ctx);
  completeDelete({ok:true,status:204}); await deletion;
  assert.match(get('delete-status').textContent, /^Удалено: \/BIG.MKV/);
  assert.equal(timers, 0);
  ctx.fetch = async () => ({ok:false,status:503});
  await assert.rejects(vm.runInContext('deleteSelection("/FAIL.MKV",false)',ctx));
  assert.match(get('delete-status').textContent, /не подтверждено/);
  assert.equal(timers, 0);
  console.log(page+': preview, popover actions, directory uploads, captured destination, recursive delete OK');
}
