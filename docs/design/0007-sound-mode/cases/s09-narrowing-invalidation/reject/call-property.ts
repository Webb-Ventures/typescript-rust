// Any call may change cfg.port.
interface Config {
  port: number | string;
}
function use(n: number): void {
  console.log(n);
}
function start(cfg: Config, log: (m: string) => void): void {
  if (typeof cfg.port === "number") {
    log("starting");
    use(cfg.port); // @expect TSR0901
  }
}
start({ port: 80 }, console.log);
export {};
