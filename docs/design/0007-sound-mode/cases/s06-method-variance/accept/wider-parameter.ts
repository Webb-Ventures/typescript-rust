// An implementation may accept more than the interface method requires.
interface AppEvent {
  readonly type: string;
}
interface Handler {
  handle(e: AppEvent): void;
}
class Tolerant implements Handler {
  handle(e: AppEvent | null): void {
    console.log(e === null ? "none" : e.type);
  }
}
const h: Handler = new Tolerant();
h.handle({ type: "x" });
export {};
