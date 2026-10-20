// `using` and `await using` lowered below ESNext behave like the native statements.
const log: string[] = [];

function res(name: string) {
    return {
        name,
        [Symbol.dispose]() {
            log.push("dispose " + name);
        },
    };
}

function asyncRes(name: string) {
    return {
        name,
        async [Symbol.asyncDispose]() {
            await null;
            log.push("async dispose " + name);
        },
    };
}

function block() {
    using a = res("a");
    {
        using b = res("b"), c = null;
        log.push("in block " + b.name + " " + c);
    }
    log.push("after block " + a.name);
    return a.name;
}

function throws() {
    try {
        using d = res("d");
        log.push("before throw " + d.name);
        throw new Error("boom");
    } catch (e) {
        log.push("caught " + (e as Error).message);
    }
}

function loops() {
    for (using r of [res("x"), res("y")]) {
        log.push("loop " + r.name);
    }
    for (using s = res("s"), t = res("t"); ; ) {
        log.push("for " + s.name + t.name);
        break;
    }
}

class Holder {
    field = "field";
    constructor(public p: string) {
        using h = res("ctor");
        log.push("ctor " + this.field + " " + this.p + " " + h.name);
    }
}

async function main() {
    await using z = asyncRes("z");
    using y = res("y2");
    log.push("main " + z.name + y.name);
    for await (using w of [res("w1"), res("w2")]) {
        log.push("for await " + w.name);
    }
}

using top = res("top");
export const exported = block();
throws();
loops();
new Holder("param");
await main();
log.push("top " + top.name + " " + exported);
console.log(log.join("\n"));
