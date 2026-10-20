import React, { useState } from 'react';

type Props<T> = { items: T[]; render: (item: T) => React.ReactNode };

export function List<T,>({ items, render }: Props<T>) {
  const [open, setOpen] = useState<boolean>(false);
  return (
    <>
      <button className="toggle" onClick={() => setOpen(o => !o)} aria-expanded={open} disabled>
        Toggle &amp; show
      </button>
      {open && (
        <ul data-count={items.length} {...{ role: 'list' }}>
          {items.map((item, i) => <li key={i}>{render(item)}</li>)}
        </ul>
      )}
      <Generic<string> value="x" />
      <ns:tag ns:attr='v' />
      <Foo.Bar.Baz />
      {/* comment child */}
    </>
  );
}

const Arrow = <T extends object>(p: T) => <div {...p} />;
