// declare namespace in a sound file.
declare namespace Analytics { // @expect TSR1103
  function track(event: string): void;
}
Analytics.track("open");
export {};
