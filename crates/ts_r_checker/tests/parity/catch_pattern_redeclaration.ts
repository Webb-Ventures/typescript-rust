try {} catch ([a, b]) {
    const [c, b] = [0, 1];
}
try {} catch ({ x, y: [z = 1, ...w] }) {
    let { p: w } = { p: 1 };
    const x = 2, z = 3;
    var y = 4;
}
try {} catch (e) {
    let e = 1;
}
export {};
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
