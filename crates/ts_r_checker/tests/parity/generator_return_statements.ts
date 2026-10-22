function* a(): Generator<number, string> { return 1; }
function* b(): Generator<number, string> { return; }
function* c(): Iterator<number, { x: 'x' }> { return { x: 'x' }; }
function* d(): Iterator<number, { x: 'x' }> { const r = { x: 'x' }; return r; }
async function* e(): AsyncGenerator<any, { x: 'x' }, any> { return Promise.resolve({ x: 'x' }); }
async function* f(): AsyncGenerator<any, { x: 'x' }, any> { const ret = { x: 'x' }; return Promise.resolve(ret); }
function* g(): Iterable<number> { return 1; }
function* h(): IterableIterator<number> { return 1; }
function* i(): Generator<number, void> { return 1; }
function* j(c: boolean): Generator<number, string> { if (c) return "a"; return c ? 1 : "b"; }
export {};
