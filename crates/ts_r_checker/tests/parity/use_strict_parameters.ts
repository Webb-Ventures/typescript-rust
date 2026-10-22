// 'use strict' with a non-simple parameter list (TS1346, TS1347) from ES2016 on.
// @filename: a.ts
function a(a = 10) {
    "use strict";
}

export var foo = 10;
function b(a = 10) {
}

function container() {
    "use strict";
    function f(a = 10) {
    }
}

function rest(...args: any[]) {
    'use strict';
}

function rest1(a = 1, ...args) {
    'use strict';
}

function paramDefault(param = 1) {
    'use strict';
}

function objectBindingPattern({foo}: any) {
    'use strict';
}

function arrayBindingPattern([foo]: any[]) {
    'use strict';
}

function manyParameter(a = 10, b = 20) {
    "use strict";
}

function manyPrologue(a = 10, b = 20) {
    "foo";
    "use strict";
}

function invalidPrologue(a = 10, b = 20) {
    "foo";
    const c = 1;
    "use strict";
}
const g = ({x}: any) => { "use strict"; };
class C { m(a = 1) { "use strict"; } constructor(...r: any[]) { 'use strict'; } set s([v]: any) { "use strict"; } }
const o = { m(this: any, a?: number) { "use strict"; } };
function esc(a = 1) { "use\x20strict"; }
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2016", "module": "ESNext", "moduleResolution": "bundler", "noEmit": true}, "files": ["a.ts"]}
