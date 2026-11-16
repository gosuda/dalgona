(async () => {
	const failures = [];
	const card = document.querySelector("section.request-card");
	if (!card) return { ok: false, failures: ["approval card: not shown"] };
	const body = card.querySelector('[aria-label="Request details"]');
	if (!body)
		return { ok: false, failures: ["request details: region not found"] };
	const viewport = `${innerWidth}x${innerHeight}`;
	if (!["320x568", "375x812"].includes(viewport))
		failures.push(`viewport: ${viewport} (want 320x568 or 375x812)`);
	const diffs = card.querySelectorAll("details.file-change").length;
	if (diffs < 3)
		failures.push(`diffs: ${diffs} shown (want at least 3 long diffs)`);
	const originalScroll = body.scrollTop;
	const box = body.getBoundingClientRect();
	if (body.clientHeight < 36 || box.height < 36)
		failures.push(
			`request details: height ${body.clientHeight} (want readable height >= 36)`,
		);
	if (box.top < 0 || box.bottom > innerHeight)
		failures.push("request details: outside the viewport");
	if (!["auto", "scroll"].includes(getComputedStyle(body).overflowY))
		failures.push("request details: not a scroll region");
	body.scrollTop = body.scrollHeight;
	await new Promise((resolve) => requestAnimationFrame(resolve));
	if (body.scrollHeight > body.clientHeight && body.scrollTop === 0)
		failures.push("request details: did not scroll");
	const controls = [...card.querySelectorAll(".request-actions button")];
	if (controls.length !== 4)
		failures.push("request actions: want all four controls");
	for (const node of [card.querySelector("h2"), ...controls]) {
		if (!node) continue;
		const rect = node.getBoundingClientRect();
		const label = node.textContent.trim();
		if (
			rect.top < 0 ||
			rect.bottom > innerHeight ||
			rect.left < 0 ||
			rect.right > innerWidth
		)
			failures.push(`${label}: outside the viewport`);
		const hit = document.elementFromPoint(
			rect.left + rect.width / 2,
			rect.top + rect.height / 2,
		);
		if (hit !== node && !node.contains(hit))
			failures.push(`${label}: occluded`);
		if (node instanceof HTMLButtonElement && rect.height < 36)
			failures.push(`${label}: target height ${rect.height} (want >= 36)`);
	}
	const composer = document.querySelector('[aria-label="Message composer"]');
	if (
		composer &&
		composer.getBoundingClientRect().top < card.getBoundingClientRect().bottom
	)
		failures.push("composer: overlaps approval evidence or actions");
	if (document.documentElement.scrollWidth > innerWidth)
		failures.push("page: horizontal overflow");
	if (document.querySelectorAll(".dock .point").length !== 1)
		failures.push("dock: want one point");
	for (const name of ["dock", "composer"]) {
		const node = document.querySelector(`.${name}`);
		if (!node) {
			failures.push(`${name}: not found`);
			continue;
		}
		if (node.scrollHeight > node.clientHeight + 1)
			failures.push(
				`${name}: scrollHeight ${node.scrollHeight} (want <= ${node.clientHeight} + 1)`,
			);
	}
	const heading = card.querySelector("#request-title");
	const step = Math.max(48, body.clientHeight - 48);
	body.scrollTop = 0;
	if (!heading) {
		failures.push("request title: not found");
	} else {
		heading.focus();
		heading.dispatchEvent(
			new KeyboardEvent("keydown", {
				key: "PageDown",
				bubbles: true,
				cancelable: true,
			}),
		);
		await new Promise((resolve) => requestAnimationFrame(resolve));
		if (Math.abs(body.scrollTop - step) > 1)
			failures.push(
				`request details: PageDown moved ${Math.round(body.scrollTop)} (want ${Math.round(step)})`,
			);
	}
	body.scrollTop = 0;
	const inside = body.querySelector("pre");
	if (!inside) {
		failures.push("pre: not found");
	} else {
		inside.dispatchEvent(
			new KeyboardEvent("keydown", {
				key: "PageDown",
				bubbles: true,
				cancelable: true,
			}),
		);
		await new Promise((resolve) => requestAnimationFrame(resolve));
		if (body.scrollTop !== 0)
			failures.push(
				`request details: PageDown on the inner pre moved ${Math.round(body.scrollTop)} (want 0)`,
			);
	}
	body.scrollTop = originalScroll;
	return { ok: failures.length === 0, failures };
})();
