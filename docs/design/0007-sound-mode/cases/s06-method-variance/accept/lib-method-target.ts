// The target method is declared in lib (EventListenerObject.handleEvent), so it stays bivariant.
const listener: EventListenerObject = {
  handleEvent(e: MouseEvent): void {
    console.log(e.clientX);
  },
};
document.body.addEventListener("click", listener);
export {};
