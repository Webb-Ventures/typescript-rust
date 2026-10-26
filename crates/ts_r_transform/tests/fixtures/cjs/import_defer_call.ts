// @target: esnext
// @module: commonjs
export async function load() {
    const a = await import.defer("./a");
    const b = await import("./b");
    return [a, b];
}
