// Directory state, local filtering and keyboard focus; plain DOM model, no dependencies.
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import assert from 'node:assert/strict';
const flush=()=>new Promise(r=>setImmediate(r));
for(const page of ['index.html','demo.html']) {
  const nodes=new Map(),calls=[];
  let document,fail=false;
  const el=(tag='div')=>({tag,value:'',textContent:'',children:[],attrs:new Map(),disabled:false,hidden:false,
    classList:{add(){},remove(){},toggle(){}},listeners:{},
    append(...items){for(const i of items)this.children.push(...(i.tag==='fragment'?i.children:[i]));},
    replaceChildren(...items){this.children=[];this.append(...items);},
    setAttribute(k,v){this.attrs.set(k,v);},getAttribute(k){return this.attrs.get(k)||null;},
    removeAttribute(k){this.attrs.delete(k);if(k==='value')this.value=0;},
    focus(){document.activeElement=this;},addEventListener(k,v){this.listeners[k]=v;},
    querySelectorAll(){const found=[];const visit=e=>{if(e.attrs?.has('data-focus-key'))found.push(e);for(const c of e.children||[])visit(c);};for(const c of this.children)visit(c);return found;},
    contains(){return false;}});
  const get=id=>{if(!nodes.has(id))nodes.set(id,el());return nodes.get(id);};
  document={getElementById:get,createElement:el,createDocumentFragment:()=>el('fragment'),activeElement:null};
  const entries=[{name:'10.jpg',size:10},{name:'2.jpg',size:20},{name:'DOCS',dir:true},{name:'.secret',size:1},{name:'.config',dir:true}];
  const ctx=vm.createContext({document,window:{addEventListener(){}},location:{pathname:'/',search:''},history:{pushState(){}},URL,URLSearchParams,TextEncoder,performance:{now:()=>0},setInterval:()=>1,clearInterval(){},
    fetch:async(url,opts={})=>{calls.push(url);return {ok:!fail,status:fail?503:200,json:async()=>url.startsWith('/api/photoframe')?{running:false,message:'idle'}:{entries:url.includes('EMPTY')?[]:entries}};}});
  const raw=readFileSync(new URL('../frontend/'+page,import.meta.url),'utf8').match(/<script>([\s\S]*?)<\/script>/)[1];
  vm.runInContext(raw.slice(raw.indexOf('const $')),ctx);await flush();
  const rows=()=>get('files').children.filter(e=>e.className==='file-row');
  const names=()=>rows().map(e=>e.children[0].children[1].children[0].textContent);
  assert.deepEqual(names(),['DOCS','2.jpg','10.jpg']);
  const requestsBeforeEye=calls.length;
  get('column-eye').onclick();
  assert.equal(get('column-eye').getAttribute('aria-pressed'),'true');
  assert(names().includes('.secret'));assert(names().includes('.config'));
  get('column-eye').onclick();
  assert.equal(get('column-eye').getAttribute('aria-pressed'),'false');
  assert.deepEqual(names(),['DOCS','2.jpg','10.jpg']);
  get('search').value='.secret';get('search').oninput();assert.equal(rows().length,0);
  get('column-eye').onclick();assert.deepEqual(names(),['.secret']);
  get('column-eye').onclick();get('search').value='';get('search').oninput();
  assert.equal(calls.length,requestsBeforeEye);assert.equal(entries.length,5);
  get('search-panel').hidden=true;
  get('search-focus').onclick();
  assert.equal(get('search-panel').hidden,false);
  assert.equal(get('search-focus').getAttribute('aria-expanded'),'true');
  assert.equal(document.activeElement,get('search'));
  get('search').value='2';get('search').oninput();
  get('search-focus').onclick();
  assert.equal(get('search-panel').hidden,true);
  assert.equal(get('search').value,'');
  assert.deepEqual(names(),['DOCS','2.jpg','10.jpg']);
  vm.runInContext("notify('Сохранено'); notify('Ошибка', true)",ctx);
  assert.equal(get('toasts').children.length,2);
  assert.equal(get('toasts').children[1].children[0].getAttribute('role'),'alert');
  get('toasts').children[0].children[1].onclick();
  assert.equal(get('toasts').children.length,1);
  get('toasts').children[0].children[1].onclick();
  assert.equal(get('toasts').children.length,0);
  const requests=calls.length;
  get('search').value='2';get('search').oninput();
  assert.deepEqual(names(),['2.jpg']);assert.equal(calls.length,requests);
  get('search').value='нет такого';get('search').oninput();
  assert.match(get('files').children[0].children[0].textContent,/Ничего не найдено/);
  get('search').value='';get('sort-name').onclick();
  assert.equal(get('sort-name-arrow').textContent,'↑');
  assert.deepEqual(names(),['DOCS','10.jpg','2.jpg']);
  get('sort-size').onclick();get('sort-size').onclick();
  assert.equal(get('sort-size-arrow').textContent,'↑');
  assert.equal(get('sort-name-arrow').textContent,'');assert.deepEqual(names(),['DOCS','2.jpg','10.jpg']);
  const folder=rows()[0].children[0].children[1].children[0];
  assert.equal(folder.tag,'a');assert.equal(folder.href,'/?path=%2FDOCS');
  folder.onclick({ctrlKey:true});assert.equal(calls.length,requests);
  folder.focus();await vm.runInContext('go("/DOCS")',ctx);
  assert.equal(document.activeElement,get('folder-heading'));
  const file=rows()[1].children[0].children[1].children[0];file.focus();
  await vm.runInContext('load()',ctx);
  assert.notEqual(document.activeElement,file);assert.equal(document.activeElement.getAttribute('data-focus-key'),'name:/DOCS/2.jpg');
  fail=true;await vm.runInContext('go("/MISSING")',ctx);
  const error=get('files').children[0];
  assert.match(error.children[0].textContent,/Не удалось/);assert.doesNotMatch(error.children[0].textContent,/Папка пуста/);
  assert.equal(get('counter').textContent,'Список недоступен');assert.equal(error.children[2].textContent,'Повторить');
  fail=false;await vm.runInContext('go("/EMPTY")',ctx);
  assert.match(get('files').children[0].children[0].textContent,/Папка пуста/);
  assert.equal(get('search').disabled,false);
  assert.equal(get('photo-state').hidden,true);
  ctx.window.P4PhotoTools={prepare:async file=>file};get('upload-progress').value=100;
  await vm.runInContext('preparePhoto({name:"next.jpg",size:5})',ctx);
  assert.equal(get('upload-progress').value,0);
  console.log(page+': local search/sort, links, focus, separate error/empty states and preparation progress OK');
}

const script = page => readFileSync(new URL('../frontend/'+page,import.meta.url),'utf8').match(/<script>([\s\S]*?)<\/script>/)[1];
const app = page => { const s=script(page); return s.slice(s.indexOf('const $')); };
assert.equal(app('index.html'),app('demo.html'),'live/demo shared UI must remain identical');
