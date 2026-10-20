// An async method with a narrower parameter.
interface AppEvent {
  readonly type: string;
}
interface ClickEvent extends AppEvent {
  readonly x: number;
}
interface Store {
  save(e: AppEvent): Promise<void>;
}
class ClickStore {
  async save(e: ClickEvent): Promise<void> {
    console.log(e.x);
  }
}
const s: Store = new ClickStore(); // @expect TSR0601
await s.save({ type: "key" });
export {};
