(() => {
	const failures = [];
	if (!matchMedia("(forced-colors: active)").matches) {
		failures.push("forced-colors: not active (emulate forced-colors=active)");
	}
	const marks = [...document.querySelectorAll(".tag, button.point")];
	if (marks.length === 0) failures.push("no tag or point button in view");
	for (const el of marks) {
		const width = Number.parseFloat(getComputedStyle(el).borderTopWidth);
		if (!(width > 0)) {
			const label = `${el.tagName.toLowerCase()}.${el.className}`;
			failures.push(`${label}: border ${width}px (want > 0)`);
		}
	}
	return { ok: failures.length === 0, failures: failures.slice(0, 50) };
})();
