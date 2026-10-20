export function g(o) {
    const t = tag `one ${o} two ${1} three`;
    return [o?.a?.b(), t];
}
export class K {
    constructor() {
        console.log(new.target);
    }
    *gen() {
        yield 1;
    }
}
//# sourceMappingURL=maps.js.map