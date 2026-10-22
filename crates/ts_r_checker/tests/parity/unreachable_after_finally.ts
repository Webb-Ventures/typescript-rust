// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"noEmit":true,"target":"ES2022","allowUnreachableCode":false},"files":["a.ts"]}
// @filename: a.ts
declare function fail(): never;
function f42(x: number) {
    try {
        x;
        fail();
        x;
    }
    finally {
        x;
    }
    x;
}
function g1(x: number) {
    try {
        fail();
    } catch {
        x;
    } finally {
        x;
    }
    x;
}
function g2(x: number) {
    try {
        try {
            fail();
        } finally {
            x;
        }
        x;
    } finally {
        x;
    }
    x;
}
function g3(x: number) {
    for (;;) {
        try {
            if (x) break;
            fail();
        } finally {
            x;
        }
        x;
    }
    x;
}
