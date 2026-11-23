(async () => {
	const failures = [];
	const threads = 'nav[aria-label="Conversation threads"] button.thread';
	const composer = () =>
		document.querySelector('[aria-label="Message composer"] textarea');
	const first = document.querySelector(threads);
	const current = document.querySelector(`${threads}[aria-current="true"]`);
	if (first && !current) {
		first.click();
		for (let i = 0; i < 100 && composer()?.disabled !== false; i++) {
			await new Promise((resolve) => setTimeout(resolve, 50));
		}
	}
	const band = document.querySelector(".status-band");
	if (!band) return { ok: false, failures: ["status band: not found"] };
	const rem = parseFloat(getComputedStyle(document.documentElement).fontSize);
	if (innerWidth > 40 * rem)
		return {
			ok: false,
			failures: [
				`viewport: ${innerWidth}x${innerHeight} (want 40rem or narrower)`,
			],
		};
	const shown = (id) => {
		const item = band.querySelector(`[data-item="${id}"]`);
		return item !== null && getComputedStyle(item).display !== "none";
	};
	for (const id of ["model", "context", "approval", "sandbox"]) {
		if (!shown(id))
			failures.push(`${id}: not visible at ${innerWidth}px (want visible)`);
	}
	const bandRem = band.getBoundingClientRect().width / rem;
	for (const [id, below] of [
		["folder", 30],
		["queue", 26],
		["effort", 16],
		["mark", 12],
	]) {
		const item = band.querySelector(`[data-item="${id}"]`);
		if (item === null) continue;
		const visible = getComputedStyle(item).display !== "none";
		const want = bandRem <= below ? "dropped" : "visible";
		if (visible !== (want === "visible"))
			failures.push(
				`${id}: ${visible ? "visible" : "dropped"} at band width ${bandRem.toFixed(2)}rem (want ${want})`,
			);
	}
	const shell = document.querySelector(".shell");
	if (shell && shell.scrollWidth > window.innerWidth) {
		failures.push(
			`shell: scrollWidth ${shell.scrollWidth} above ${window.innerWidth}`,
		);
	}
	return { ok: failures.length === 0, failures };
})();
