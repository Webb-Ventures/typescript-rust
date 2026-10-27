// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "es2015", "noEmit": true, "allowJs": true, "checkJs": true, "types": []}, "files": ["index.js"]}
// @filename: index.js
module.exports.b = function b() {};
/**
 * @param {{x: string}} a
 * @param {{y: typeof module.exports.b}} b
 */
function g(a, b) {
    return a.x && b.y();
}
/** @type {number} */
const z = g;
module.exports.g = g;
