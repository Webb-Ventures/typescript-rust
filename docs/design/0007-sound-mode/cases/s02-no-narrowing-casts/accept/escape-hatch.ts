// A narrowing cast with a stated reason.
interface Frame {
  readonly kind: "ping";
}
function decode(data: unknown): Frame {
  // @ts-r-unsafe S-02: the transport validates every frame before it reaches this function.
  return data as Frame;
}
export const f: Frame = decode({ kind: "ping" });
