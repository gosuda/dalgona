(async () => {
	const failures = [];
	if (!matchMedia("(forced-colors: active)").matches) {
		return { ok: false, failures: ["media: forced-colors is not active"] };
	}
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
	const rows = [...document.querySelectorAll(threads)];
	if (rows.length < 2)
		return {
			ok: false,
			failures: [`threads: ${rows.length} shown (want at least 2)`],
		};
	const selected = rows.filter(
		(row) => row.getAttribute("aria-current") === "true",
	);
	if (selected.length !== 1)
		failures.push(`selected rows: ${selected.length} (want 1)`);
	for (const row of rows) {
		const want = row.getAttribute("aria-current") === "true" ? "2px" : "0px";
		const width = getComputedStyle(row).borderInlineStartWidth;
		if (width !== want)
			failures.push(
				`${row.querySelector(".title")?.textContent.trim()}: border-inline-start ${width} (want ${want})`,
			);
	}
	if (selected.length === 1) {
		const other = rows.find((row) => row !== selected[0]);
		const selectedWeight = getComputedStyle(
			selected[0].querySelector(".title"),
		).fontWeight;
		const otherWeight = getComputedStyle(
			other.querySelector(".title"),
		).fontWeight;
		if (selectedWeight === otherWeight)
			failures.push(
				`title font weight: selected ${selectedWeight} equals other ${otherWeight}`,
			);
	}
	return { ok: failures.length === 0, failures };
})();
