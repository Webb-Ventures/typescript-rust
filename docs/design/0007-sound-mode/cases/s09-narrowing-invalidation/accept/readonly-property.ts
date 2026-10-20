// A readonly property keeps its narrowing across calls.
interface Frozen {
  readonly port: number | undefined;
}
function use(n: number): void {
  console.log(n);
}
function start(cfg: Frozen): void {
  if (cfg.port !== undefined) {
    console.log("starting");
    use(cfg.port);
  }
}
start({ port: 80 });
export {};
