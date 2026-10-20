// A JS file with CommonJS indicators is a module.
const fs = require("fs");
function helper() { return fs; }
module.exports = { helper };
exports.extra = 1;
module.exports.more = 2;
exports["quoted"] = 3;
exports.extra = undefined;
module.exports = exports;
Object.defineProperty(exports, "dp", { value: 1 });
Object.defineProperty(module.exports, "rw", { value: 1, writable: true });
