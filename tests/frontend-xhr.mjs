// Keep existing app integration tests on the same deterministic fetch transport.
export function xhrAdapter(fetch) {
  return class {
    constructor() { this.upload = {}; }
    open(method, url) { this.method = method; this.url = url; }
    setRequestHeader() {}
    send(body) {
      Promise.resolve(fetch(this.url, {method: this.method, body})).then(response => {
        if (this.aborted) return;
        this.upload.onprogress?.({loaded:body.size,total:body.size,lengthComputable:true});
        this.upload.onload?.(); this.status = response.status; this.onload?.();
      }, () => this.onerror?.());
    }
    abort() { this.aborted = true; this.onabort?.(); }
  };
}
