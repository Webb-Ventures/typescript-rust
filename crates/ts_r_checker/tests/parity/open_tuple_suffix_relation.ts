// Open-ended tuple sources relate their suffix to a target suffix.
export {};
declare const ss: string[];
declare const r: [...string[], 'b', 1];
const q: [...string[], 'a', 1] = r;
const q2: [...string[], 'a', 1] = [...ss, 'b', 1];
const q3: [...string[], 'a'] = [...ss, 'b'];
