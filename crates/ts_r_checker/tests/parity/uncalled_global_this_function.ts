// A function tested through `globalThis` and called the same way in the body is used.
export function f(error: unknown, flag = true): void {
  if (flag && globalThis.reportError) {
    globalThis.reportError(error);
  }
  if (globalThis.reportError) {
    globalThis.reportError(error);
  }
  if (globalThis.reportError) {
    reportError(error);
  }
  if (flag && reportError) {
    reportError(error);
  }
  if (globalThis.reportError) {
  }
  if (window.reportError) {
    window.reportError(error);
  }
}

// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ESNext", "lib": ["ESNext", "DOM"], "module": "nodenext", "moduleResolution": "nodenext", "noEmit": true}, "files": ["a.ts"]}
