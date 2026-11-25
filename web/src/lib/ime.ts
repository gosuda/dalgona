interface Composition {
	active: boolean;
	readonly heldKeys: Set<string>;
}

const compositions = new WeakMap<EventTarget, Composition>();

function compositionOf(target: EventTarget): Composition {
	const known = compositions.get(target);
	if (known !== undefined) return known;
	const created: Composition = { active: false, heldKeys: new Set() };
	compositions.set(target, created);
	return created;
}

function keyIdentity(event: KeyboardEvent): string {
	return event.code === "" ? event.key : event.code;
}

function signalsComposition(event: KeyboardEvent): boolean {
	return event.isComposing || event.key === "Process" || event.keyCode === 229;
}

export function isImeKey(event: KeyboardEvent): boolean {
	if (signalsComposition(event)) return true;
	if (event.target === null) return false;
	const composition = compositions.get(event.target);
	if (composition === undefined) return false;
	return composition.active || composition.heldKeys.has(keyIdentity(event));
}

function startComposition(event: Event): void {
	if (event.target !== null) compositionOf(event.target).active = true;
}

function endComposition(event: Event): void {
	if (event.target !== null) compositionOf(event.target).active = false;
}

function holdComposedKey(event: KeyboardEvent): void {
	if (event.target === null || !isImeKey(event)) return;
	compositionOf(event.target).heldKeys.add(keyIdentity(event));
}

function releaseKey(event: KeyboardEvent): void {
	if (event.target === null) return;
	compositions.get(event.target)?.heldKeys.delete(keyIdentity(event));
}

function releaseAllKeys(event: FocusEvent): void {
	if (event.target === null) return;
	compositions.get(event.target)?.heldKeys.clear();
}

if (typeof document !== "undefined") {
	document.addEventListener("compositionstart", startComposition, true);
	document.addEventListener("compositionend", endComposition, true);
	document.addEventListener("keydown", holdComposedKey, true);
	document.addEventListener("keyup", releaseKey, true);
	document.addEventListener("focusout", releaseAllKeys, true);
}
