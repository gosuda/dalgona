(async () => {
	const failures = [];
	const elapsed = () =>
		document.querySelector(".thread-head .thread-title > .tag.live .elapsed")
			?.textContent ?? "";
	const rowCount = () =>
		document.querySelectorAll("#transcript ol.items > li").length;
	const before = elapsed();
	const beforeRows = rowCount();
	const animations = document.getAnimations().length;
	if (animations !== 0) failures.push(`animations: ${animations} (want 0)`);
	await new Promise((resolve) => setTimeout(resolve, 3000));
	const after = elapsed();
	const afterRows = rowCount();
	if (before === "")
		failures.push("no live tag in the thread header: a turn must be running");
	if (afterRows !== beforeRows) {
		failures.push(
			`inconclusive: new item (rows ${beforeRows} to ${afterRows}), so the counter may refresh`,
		);
	}
	if (after !== before) {
		failures.push(`elapsed text: "${after}" (want "${before}" unchanged)`);
	}
	return { ok: failures.length === 0, failures: failures.slice(0, 50) };
})();
