type R<T> = { isSuccess: false; data: undefined } | { isSuccess: true; data: T };
declare function useQuery(): R<number>;

function direct({ data: data1, isSuccess: isSuccess1 }: R<number>) {
    if (isSuccess1) {
        data1.toExponential();
    }
}

function aliased({ data: data1, isSuccess: isSuccess1 }: R<number>) {
    const ok = isSuccess1;
    if (ok) {
        data1.toExponential();
    }
}

function both({ data: data1, isSuccess: isSuccess1 }: R<number>) {
    const { data: data2, isSuccess: isSuccess2 } = useQuery();
    const ok = isSuccess1 && isSuccess2;
    if (ok) {
        data1.toExponential();
        data2.toExponential();
    }
}

function reassigned({ data: data1, isSuccess: isSuccess1 }: R<number>) {
    const ok = isSuccess1;
    if (ok) {
        data1.toExponential();
    }
    isSuccess1 = true;
}
export {};
