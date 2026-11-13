// Run with node tests/frontend-uploads.mjs; no browser or npm dependencies.
import {readFileSync} from 'node:fs';
import {gunzipSync} from 'node:zlib';
import vm from 'node:vm';
import assert from 'node:assert/strict';

const flush = () => new Promise(resolve => setImmediate(resolve));
for (const page of ['index.html', 'demo.html']) {
  const nodes = new Map();
  function element() {
    return {
      value: '', disabled: false, textContent: '', listeners: {},
      classList: {add() {}, remove() {}, toggle() {}},
      append() {}, replaceChildren() {}, setAttribute() {}, contains() {return false;},
      addEventListener(name, fn) {this.listeners[name] = fn;},
    };
  }
  const get = id => {
    if (!nodes.has(id)) nodes.set(id, element());
    return nodes.get(id);
  };
  let active = 0, peak = 0, holdList = false, releaseList;
  const requests = [], pending = [];
  const sandbox = vm.createContext({
    document: {getElementById: get, createElement: element, createDocumentFragment: element},
    window: {addEventListener() {}}, location: {search: '', pathname: '/'},
    history: {pushState() {}}, URL, URLSearchParams, TextEncoder,
    fetch(url, opts = {}) {
      if (opts.method !== 'PUT') {
        const response = {ok: true, json: async () => ({entries: []})};
        if (holdList) {
          holdList = false;
          return new Promise(resolve => {releaseList = () => resolve(response);});
        }
        return Promise.resolve(response);
      }
      active++; peak = Math.max(peak, active);
      requests.push({url, opts});
      return new Promise(resolve => pending.push(status => {
        active--; resolve({ok: status >= 200 && status < 300, status});
      }));
    },
  });
  const script = readFileSync(new URL('../frontend/' + page, import.meta.url), 'utf8')
    .match(/<script>([\s\S]*?)<\/script>/)[1];
  // Use the real shared app, replacing only demo's in-memory fetch adapter.
  vm.runInContext(script.slice(script.indexOf('const $')), sandbox);
  await flush();
  const file = name => ({name, size: 123});
  const a = file('A.TXT'), b = file('B.BIN'), c = file('C.TXT'), d = file('D.TXT');
  get('picker').onchange({target: {files: [a, b]}});
  vm.runInContext('current = "/DOCS"', sandbox);
  const drop = files => get('panel').listeners.drop({preventDefault() {}, dataTransfer: {files}});
  drop([c]);
  assert.equal(requests.length, 1);
  assert.equal(requests[0].opts.body, a);
  pending.shift()(201); await flush();
  assert.equal(requests[1].url, '/fs/B.BIN');
  pending.shift()(500); await flush(); // One failure must not discard queued files.
  assert.equal(requests[2].url, '/fs/DOCS/C.TXT');
  vm.runInContext('current = "/OTHER"', sandbox);
  holdList = true;
  pending.shift()(201); await flush();
  assert.equal(typeof releaseList, 'function');
  drop([d]); // Race: the previous batch is waiting for its final catalog refresh.
  releaseList(); await flush();
  assert.equal(requests[3].url, '/fs/OTHER/D.TXT');
  pending.shift()(201); await flush();
  assert.equal(peak, 1);
  assert.equal(active, 0);
  assert.equal(get('refresh').disabled, false);
  assert.match(get('status').textContent, /Загружено файлов: 3.*Ошибок: 1/);
  const good = file('GOOD.BIN');
  drop([file('bad:name.txt'), {name: 'HUGE.BIN', size: 0x100000000}, good]);
  assert.equal(requests.length, 5); // Invalid files never send their body.
  assert.equal(requests[4].opts.body, good);
  pending.shift()(201); await flush();
  assert.match(get('status').textContent, /Загружено файлов: 1.*Ошибок: 2/);
  for (const [name, expected] of [['a.txt', true], ['$ABC_1.A', true], ['long-name.txt', true],
    ['a..b', true], ['x.', false], ['a b.txt', true], ['файл.txt', true], ['a\\b.txt', false]]) {
    assert.equal(vm.runInContext('validName(' + JSON.stringify(name) + ')', sandbox), expected, name);
  }
  console.log(page + ': sequential PUT, captured paths, error continuation, refresh race, FAT32 limits OK');
}
assert.deepEqual(gunzipSync(readFileSync(new URL('../frontend/index.html.gz', import.meta.url))),
  readFileSync(new URL('../frontend/index.html', import.meta.url)));
console.log('gzip matches index.html');
