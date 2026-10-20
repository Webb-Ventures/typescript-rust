// A side-effect import of untyped JavaScript is not TS7016 (playwright's
// `import 'codemirror/mode/css/css'`).
import 'foo/lib/side';
import 'foo';
export {};
// @filename: node_modules/foo/package.json
{"name": "foo", "version": "1.0.0", "main": "lib/index.js"}
// @filename: node_modules/foo/lib/index.js
module.exports = {};
// @filename: node_modules/foo/lib/side.js
module.exports = {};
