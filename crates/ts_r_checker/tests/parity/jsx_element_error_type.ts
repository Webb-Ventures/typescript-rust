// @filename: a.tsx
// Without a global `JSX` namespace (React's types not installed) a JSX element has the
// error type: arithmetic on it gives `number`, `keyof` it is `keyof any`, and a function
// returning one returns the error type.
const el = <div />
const n = el * 2
const s: string = n
const k: keyof typeof el = true
function f() {
  return <></>
}
const r: number = f()
const t: string = f() * 1
export {}
