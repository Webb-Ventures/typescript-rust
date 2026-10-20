// ts-r guard runtime (record 0007, "parse<T> runtime validators"): the helpers compiled
// predicates call, and the descriptor walker that builds a ParseError after a predicate fails.
// Every top-level item declares its own names, starts at column 0 and is unindented only
// there, so `runtime_for` (inline output) and bundlers (shared output) keep just the items
// the generated code reaches: a type without unions never ships the union walker.
// The error classes are created on first use and shared through the global symbol registry,
// so the copies of the runtime inlined into different files (ts-r-run, ts-r-dev) agree on
// `instanceof`. Each is a getter (`$ParseError()`), so an unused class costs nothing.
const $shared = (name, make) => (globalThis[Symbol.for("ts-r." + name)] ??= make());
const $ParseError = () => $shared("ParseError", () => class ParseError extends Error {
  constructor(type, issues) {
    super($message(type, issues));
    this.name = "ParseError";
    this.type = type;
    this.issues = issues;
  }
});
// `Invalid User: $.roles[0].name: expected string, received number (+2 more)`.
const $message = (type, issues, i = issues[0]) => "Invalid " + type + (i ? ": " + i.path.reduce((s, k) =>
  s + (/^[a-z_$][\w$]*$/i.test(k) ? "." + k : `[${JSON.stringify(k)}]`), "$") +
  `: expected ${i.expected}, received ${i.received}` + (issues[1] ? ` (+${issues.length - 1} more)` : "") : "");
// Predicate helpers (the fast path).
const $has = Object.hasOwn;
const $obj = (v) => typeof v == "object" && v !== null && !Array.isArray(v);
const $isArr = (v, f, d, i = 0) => {
  if (!Array.isArray(v)) return false;
  for (; i < v.length; i++) if (!f(v[i], d)) return false;
  return true;
};
const $rec = (v, f, d) => {
  for (const k in v) if ($has(v, k) && !f(v[k], d)) return false;
  return true;
};
const $isMap = (v, kf, vf, d) => {
  if (!(v instanceof Map)) return false;
  for (const [k, x] of v) if (!kf(k, d) || !vf(x, d)) return false;
  return true;
};
const $isSet = (v, f, d) => {
  if (!(v instanceof Set)) return false;
  for (const x of v) if (!f(x, d)) return false;
  return true;
};
const $str = (v) => typeof v == "string";
const $num = (v) => typeof v == "number";
const $bool = (v) => typeof v == "boolean";
// Descriptors (the error path): a primitive is its name ("string", "null", ..) and is checked
// against `$kind`; anything else is `[kind, ...data]`, where `kind(d, v, depth)` walks `v`
// and `kind(d)` names the type.
const $kind = (v) => v === null ? "null" : Array.isArray(v) ? "array" : typeof v;
const $show = (d) => typeof d == "string" ? d : d[0](d);
const $deep = "a value nested too deeply";
// The walk in progress: path, issues, issue limit.
let $P, $I, $K;
const $bad = (v, expected, received) =>
  ($I.length < $K && $I.push({ path: $P.slice(), expected, received: received || $kind(v) }), false);
const $w = (v, d, p) => $I.length < $K && (typeof d == "string"
  ? $kind(v) == d || $bad(v, d)
  : d[0](d, v, p));
const $at = (k, f) => {
  $P.push(k);
  const ok = f();
  $P.pop();
  return ok;
};
const $each = (xs, f, p, ok = true, i = 0) => {
  for (const x of xs) {
    const j = i++;
    ok = $at(j, () => $w(x, f(j), p)) && ok;
  }
  return ok;
};
// An object's properties: `[key, descriptor]` pairs; `0` before a key marks it optional.
const $props = (d) => {
  const out = [], a = d[1];
  for (let i = 0; i < a.length; ) {
    const o = a[i] === 0 && ++i;
    out.push([a[i++], a[i++], o]);
  }
  return out;
};
const $O = (d, v, p) => {
  if (p == null) return "object";
  if (!$obj(v)) return $bad(v, "object");
  let ok = true;
  for (const [k, t, o] of $props(d)) ok = $at(k, () => $has(v, k) ? $w(v[k], t, p) : o || $bad(v, $show(t), "missing")) && ok;
  if (d[2]) for (const k of Object.keys(v)) ok = $at(k, () => $w(v[k], d[2], p)) && ok;
  return ok;
};
const $Any = (d, v, p) => p == null ? "unknown" : true;
const $A = (d, v, p) => p == null ? "array" : Array.isArray(v) ? $each(v, () => d[1], p) : $bad(v, "array");
const $L = (d, v, p) => p == null
  ? d.slice(1).map((x) => typeof x == "bigint" ? x + "n" : JSON.stringify(x)).join(" | ")
  : d.includes(v, 1) || $bad(v, $show(d));
const $R = (d, v, p) => p == null ? d[1] : p < $MAX_DEPTH ? $w(v, d[2].apply(0, d[3]), p + 1) : $bad(v, d[1], $deep);
// `[$C, name]` for a global class (`Date`), `[$C, name, C]` for an imported user class.
const $C = (d, v, p, C = d[2] || globalThis[d[1]]) => p == null ? d[1] : (typeof C == "function" && v instanceof C) || $bad(v, d[1]);
// A refined type `[$F, base, predicate, name]`: the base type, then the registered predicate.
const $F = (d, v, p) => p == null ? d[3] : $w(v, d[1], p) && (d[2](v) || $bad(v, d[3]));
const $X = (d, v, p) => p == null ? d[2] : (typeof v == "string" && d[1].test(v)) || $bad(v, d[2]);
const $T = (d, v, p) => {
  if (p == null) return "tuple";
  const [, els, req, rest] = d, n = els.length;
  return !Array.isArray(v) ? $bad(v, "tuple")
    : v.length < req || (!rest && v.length > n)
      ? $bad(v, rest ? `tuple of at least ${req} elements` : req == n ? `tuple of ${req} elements` : `tuple of ${req} to ${n} elements`, v.length + " elements")
      : $each(v, (i) => i < n ? els[i] : rest, p);
};
const $Map = (d, v, p) => {
  if (p == null) return "Map";
  if (!(v instanceof Map)) return $bad(v, "Map");
  let ok = true;
  for (const [k, x] of v) ok = $at(String(k), () => $w(k, d[1], p) && $w(x, d[2], p)) && ok;
  return ok;
};
const $Set = (d, v, p) => p == null ? "Set" : v instanceof Set ? $each(v, () => d[1], p) : $bad(v, "Set");
// `d` with issues collected separately, at most one: does `v` match it?
const $probe = (v, d, p) => {
  const I = $I, K = $K;
  $I = [];
  $K = 1;
  const ok = $w(v, d, p);
  $I = I;
  $K = K;
  return ok;
};
const $join = (ms) => [...new Set(ms.map($show))].join(" | ");
const $deepIf = (v, p) => p >= $MAX_DEPTH && typeof v == "object" && v !== null ? $deep : undefined;
// A union without object members: the first member that matches.
const $U = (d, v, p) => p == null ? $join(d.slice(1)) : d.slice(1).some((m) => $probe(v, m, p)) || $bad(v, $show(d), $deepIf(v, p));
// A discriminated union `[$D, key, tags, member, tags, member, ..]` (tags: a literal or an
// array of them): the member whose tag the value's `key` has, else an issue at `key`.
const $D = (d, v, p) => {
  const ts = [], ms = [];
  for (let i = 2; i < d.length; i += 2) ts.push([].concat(d[i])), ms.push(d[i + 1]);
  if (p == null) return $join(ms);
  if (!$obj(v) || p >= $MAX_DEPTH) return $bad(v, $show(d), $deepIf(v, p));
  const k = d[1], i = ts.findIndex((t) => $has(v, k) && t.includes(v[k]));
  return i < 0
    ? $at(k, () => $bad(v, $show([$L].concat(...ts)), $has(v, k) ? $kind(v[k]) : "missing"))
    : $w(v, ms[i], p + (ms[i][0] != $R));
};
// Any other union with object members. Members that are not object types first. Object
// members are walked once, in the member the value matches best, so a recursive union stays
// linear in the value.
const $V = (d, v, p) => {
  const ms = d.slice(1);
  if (p == null) return $join(ms);
  const res = ms.map((m) => m[0] == $R && p < $MAX_DEPTH ? m[2].apply(0, m[3]) : m);
  const isObj = (m) => m[0] == $O;
  for (let i = 0; i < ms.length; i++) if (!isObj(res[i]) && $probe(v, ms[i], p)) return true;
  if ($obj(v) && p < $MAX_DEPTH) {
    // Report inside the object member the value matches best: the only one, or the only
    // one whose literal properties match.
    const objs = res.filter(isObj);
    const lits = (o) => $props(o).filter(([, t]) => t[0] == $L);
    const fits = objs.filter((o) => lits(o).every(([k, t]) => $has(v, k) && t.includes(v[k], 1)));
    const best = objs.length == 1 ? objs[0] : fits.length == 1 ? fits[0] : null;
    if (best) return $w(v, best, p + 1);
    for (const o of objs) if ($probe(v, o, p + 1)) return true;
    const key = objs.length > 1 && lits(objs[0]).map(([k]) => k).find((k) => objs.every((o) => lits(o).some(([j]) => j === k)));
    if (key !== undefined && key !== false)
      return $at(key, () => $bad(v, $show([$L].concat(...objs.map((o) => lits(o).find(([j]) => j === key)[1].slice(1)))),
        $has(v, key) ? $kind(v[key]) : "missing"));
  }
  return $bad(v, $show(d), $deepIf(v, p));
};
// `v` itself when `is` accepts it; else a ParseError with up to `max` issues found by walking
// descriptor `d()` (or one for the whole value when the walk finds none). The walk in progress
// is saved and restored: a getter it reads may call parse itself.
function $parse(v, is, d, type, max = 20) {
  if (is(v, 0)) return v;
  const saved = [$P, $I, $K];
  try {
    $P = [];
    $I = [];
    $K = max;
    $w(v, d(), 0);
    $I.length || $bad(v, type);
    throw new ($ParseError())(type, $I);
  } finally {
    [$P, $I, $K] = saved;
  }
}
// `[sound.guard] explain = "none"`: no descriptors, no walker (`Invalid User`).
function $parse0(v, is, type) {
  if (is(v, 0)) return v;
  throw new ($ParseError())(type, []);
}
// safeParse: `is` first (a valid value costs what `is` costs); then `f` (parse) throws. The
// error is a value, not thrown, so it captures no stack trace (capturing one is 95% of the
// cost of an invalid input).
const $safe = (f, v, is, l) => {
  if (is(v, 0)) return { ok: true, value: v };
  l = Error.stackTraceLimit;
  Error.stackTraceLimit = 0;
  try {
    return { ok: true, value: f(v) };
  } catch (e) {
    if (e instanceof $ParseError()) return { ok: false, error: e };
    throw e;
  } finally {
    Error.stackTraceLimit = l;
  }
};
// The descriptor a named reference stands for; for a refinement, its base (inputs convert
// by the base type; the predicate runs when the result is validated).
const $deref = (d) => d[0] == $R ? $deref(d[2].apply(0, d[3])) : d[0] == $F ? $deref(d[1]) : d;
// env<E>(), query<T>(req), args<T>(): string inputs. Each declared key (every key, for an
// index signature) is read once from `src` and converted by its descriptor (numbers,
// booleans, literals; a key typed as an array collects its values); the result is then
// validated as one object. Nothing else is ever converted.
const $env = (src, d, out = {}) => {
  d = $deref(d);
  if (d[0] != $O) return src;
  for (const [k, t] of $props(d)) if (src[k] !== undefined) out[k] = $conv(src[k], t);
  if (d[2]) for (const k in src) if ($has(src, k) && !$has(out, k)) out[k] = $conv(src[k], d[2]);
  return out;
};
const $conv = (s, d, p = 0) => {
  d = $deref(d);
  // A recursive array type (`type Nest = Nest[]`) would wrap a string forever: past the depth
  // limit it stays as is, and validation reports it.
  if (d[0] == $A) return p < $MAX_DEPTH ? [].concat(s).map((x) => $conv(x, d[1], p + 1)) : s;
  if (typeof s != "string") return s;
  if (d == "number") return /^-?\d+(\.\d+)?$/.test(s) ? Number(s) : s;
  if (d == "boolean") return s == "true" || s == "1" ? true : s == "false" || s == "0" ? false : s;
  if (d[0] == $L) {
    for (let i = 1; i < d.length; i++) if (String(d[i]) === s) return d[i];
  } else if (d[0] == $U || d[0] == $V) {
    for (let i = 1; i < d.length; i++) {
      const c = $conv(s, d[i], p);
      if (c !== s) return c;
    }
  }
  return s;
};
// query<T>(req): `req.query` when a framework parsed it (Express, Fastify, Koa's
// `ctx.request`), else the search params of `req.url` (fetch `Request`, `node:http`, Hono's
// `c.req`), a URL or a URLSearchParams. A repeated key gives an array.
const $query = (r, q = r.query) => {
  if (q && typeof q == "object") return q;
  const out = { __proto__: null };
  for (const [k, v] of r instanceof URLSearchParams ? r : new URL(r.url ?? r, "http://_").searchParams)
    out[k] = $has(out, k) ? [].concat(out[k], v) : v;
  return out;
};
// args<T>(argv = process.argv.slice(2)): `--name value`, `--name=value`, `-n value`, `--flag`
// (true when the property is boolean or no value follows), `--no-flag` (false). Names match a
// property as written or in camelCase (`--dry-run` sets `dryRun`). Other words, and every word
// after `--`, go to `_`. Repeated names collect into arrays; values convert like env<E>().
const $args = (d, a = globalThis.process?.argv.slice(2) ?? [], out = { __proto__: null, _: [] }) => {
  const ts = new Map((d = $deref(d))[0] == $O ? $props(d) : []);
  const name = (k, c = k.replace(/-(\w)/g, (_, x) => x.toUpperCase())) => ts.has(k) || !ts.has(c) ? k : c;
  const flag = /^--?([a-z_][^=]*)(?:=([^]*))?$/i;
  for (let i = 0, m; i < a.length; i++) {
    if (a[i] == "--") {
      out._.push(...a.slice(i + 1));
      break;
    }
    if (!(m = flag.exec(a[i]))) {
      out._.push(a[i]);
      continue;
    }
    let k = name(m[1]), v = m[2];
    if (v === undefined) {
      const no = /^no-/.test(k) && !ts.has(k) && name(k.slice(3));
      if (ts.has(no)) (k = no), (v = "false");
      else v = (ts.has(k) && $deref(ts.get(k))) == "boolean" || i + 1 == a.length || flag.test(a[i + 1]) ? "true" : a[++i];
    }
    out[k] = $has(out, k) ? [].concat(out[k], v) : v;
  }
  return $env(out, d);
};
const $ExpectError = () => $shared("ExpectError", () => class ExpectError extends Error {
  constructor(message) {
    super(message);
    this.name = "ExpectError";
  }
});
const $PanicError = () => $shared("PanicError", () => class PanicError extends Error {
  constructor(message) {
    super(message);
    this.name = "PanicError";
  }
});
const $expect = (v, message) => {
  if (v === null || v === undefined) throw new ($ExpectError())(message);
  return v;
};
const $panic = (message) => {
  throw new ($PanicError())(message);
};
