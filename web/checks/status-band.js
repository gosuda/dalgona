(async () => {
	const failures = [];
	const band = document.querySelector(".status-band");
	if (!band) return { ok: false, failures: ["status band: not found"] };
	const shown = (id) => {
		const item = band.querySelector(`[data-item="${id}"]`);
		return item !== null && getComputedStyle(item).display !== "none";
	};
	for (const id of ["model", "context", "approval", "sandbox"]) {
		if (!shown(id)) failures.push(`${id}: not visible at 320px (want visible)`);
	}
	for (const id of ["folder", "queue"]) {
		if (shown(id)) failures.push(`${id}: visible at 320px (want dropped)`);
	}
	const shell = document.querySelector(".shell");
	if (shell && shell.scrollWidth > window.innerWidth) {
		failures.push(`shell: scrollWidth ${shell.scrollWidth} above ${window.innerWidth}`);
	}
	return { ok: failures.length === 0, failures };
})()
