declare function tag(s: TemplateStringsArray, ...v: unknown[]): string;
export function g(o?: { a?: { b(): number } }): unknown[] {
    const t = tag`one ${o} two ${1} three`;
    return [o?.a?.b(), t];
}
export class K {
    constructor() {
        console.log(new.target);
    }
    *gen(): any {
        yield 1;
    }
}
