// Control flow: branches, loops, labels, try/finally, short-circuit, IIFE inlined.
function f(x: number | string, y?: boolean) {
    if (typeof x === "string") {
        x = x.length;
    } else if (y && x > 1) {
        return x;
    }
    outer: while (x < 10) {
        for (const k of [1, 2]) {
            if (k === 2) continue outer;
            if (k === 3) break outer;
        }
        x++;
    }
    switch (x) {
        case 1: x = 2;
        case 2: break;
        default: return 0;
    }
    try { x = 3; } catch { x = 4; } finally { y = !y; }
    (() => { x = 5; })();
    return x ?? y;
}
