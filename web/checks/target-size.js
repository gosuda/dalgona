(() => {
	const failures = [];
	const coarse = matchMedia("(pointer: coarse)").matches;
	const root = getComputedStyle(document.documentElement);
	const control = Number.parseFloat(root.getPropertyValue("--control-height"));
	const coarseControl = Number.parseFloat(
		root.getPropertyValue("--control-height-coarse"),
	);
	const want = coarse ? coarseControl : control;
	const selector = "button, a[href], input, select, summary, [role=option]";
	for (const el of document.querySelectorAll(selector)) {
		if (el.classList.contains("skip-link") && document.activeElement !== el) {
			continue;
		}
		const box = el.getBoundingClientRect();
		if (box.width === 0 || box.height === 0) continue;
		const label = `${el.tagName.toLowerCase()}${el.textContent ? ` "${el.textContent.trim().slice(0, 30)}"` : ""}`;
		const size = `${Math.round(box.width)}x${Math.round(box.height)}`;
		if (box.width < 24 || box.height < 24) {
			failures.push(`${label}: ${size}px (want >= 24x24)`);
		}
		const isControl = el.tagName === "BUTTON" || el.tagName === "SELECT";
		if (isControl && box.height < want - 0.5) {
			failures.push(`${label}: ${size}px tall (want >= ${want}px)`);
		}
	}
	return { ok: failures.length === 0, failures: failures.slice(0, 50) };
})();
