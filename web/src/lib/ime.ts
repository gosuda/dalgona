export function isImeKey(event: KeyboardEvent): boolean {
	if (event.isComposing) return true;
	if (event.key === "Process") return true;
	return (event as KeyboardEvent & { keyCode?: number }).keyCode === 229;
}
