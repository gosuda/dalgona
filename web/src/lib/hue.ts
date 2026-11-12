const offsetBasis = 2166136261;
const prime = 16777619;

export function threadHue(id: string): 0 | 1 | 2 | 3 {
	let hash = offsetBasis;
	for (const byte of new TextEncoder().encode(id)) {
		hash = Math.imul(hash ^ byte, prime) >>> 0;
	}
	return (hash % 4) as 0 | 1 | 2 | 3;
}
