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
	const dock = document.querySelector(".dock");
	if (!dock) failures.push("dock: not found");
	else if (dock.scrollHeight > dock.clientHeight + 1)
		failures.push(
			`dock: scrollHeight ${dock.scrollHeight} (want <= ${dock.clientHeight} + 1)`,
		);
	if (!composer) {
		failures.push("composer: not found");
	} else {
		const frame = composer.getBoundingClientRect();
		if (frame.top < 0 || frame.bottom > innerHeight + 1)
			failures.push(
				`composer: ${Math.round(frame.top)}..${Math.round(frame.bottom)} outside 0..${innerHeight}`,
			);
		if (
			composer.scrollHeight > composer.clientHeight + 1 &&
			!["auto", "scroll"].includes(getComputedStyle(composer).overflowY)
		)
			failures.push(
				`composer: scrollHeight ${composer.scrollHeight} over clientHeight ${composer.clientHeight} but not a scroll region`,
			);
		const composerScroll = composer.scrollTop;
		for (const node of composer.querySelectorAll("label, textarea, button")) {
			composer.scrollTop += node.getBoundingClientRect().top - frame.top;
			const rect = node.getBoundingClientRect();
			const label = node.textContent.trim() || node.tagName;
			if (rect.top < frame.top - 1 || rect.bottom > frame.bottom + 1) {
				failures.push(
					`composer: ${label} cannot scroll into view (${Math.round(rect.top)}..${Math.round(rect.bottom)} outside ${Math.round(frame.top)}..${Math.round(frame.bottom)})`,
				);
				continue;
			}
			const hit = document.elementFromPoint(
				rect.left + rect.width / 2,
				rect.top + rect.height / 2,
			);
			if (hit !== node && !node.contains(hit))
				failures.push(`composer: ${label} occluded`);
		}
		composer.scrollTop = composerScroll;
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
