// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"module":"node16","resolveJsonModule":true,"noEmit":true},"files":["main.mts"]}
// @filename: node_modules/actually-json/package.json
{"name":"actually-json","version":"1.0.0","type":"module","exports":{".":"./index.json","./typed":"./typed.d.json.ts"}}
// @filename: node_modules/actually-json/index.json
{}
// @filename: node_modules/actually-json/typed.d.json.ts
declare const _default: {};
export default _default;
// @filename: main.mts
import typed from "actually-json/typed";
export const t = typed;
