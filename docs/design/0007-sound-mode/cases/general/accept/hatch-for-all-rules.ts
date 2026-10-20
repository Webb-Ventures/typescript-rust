// A hatch without rule ids covers every sound-mode diagnostic on the next line.
interface Frame {
  readonly kind: "ping";
}
const frames = new Map<string, unknown>([["a", { kind: "ping" }]]);
// @ts-r-unsafe: frames are validated when they are stored, and "a" is always present.
const frame = frames.get("a")! as Frame;
export const kind: string = frame.kind;
